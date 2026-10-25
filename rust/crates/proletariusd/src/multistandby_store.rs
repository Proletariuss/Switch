//! Profile-scoped durable catalog, inbox and outbox. Only this Rust module
//! writes the snapshot; all related indexes commit in the same atomic file.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use proletarius_forwarding::ForwardingMessage;
use proletarius_sms::stable_digest_hex;
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub(crate) const INTERNAL_PREFIX: &str = "/api/internal/multistandby/store/";
pub(crate) const INTERNAL_HEADER: &str = "x-switch-internal-token";
const MAX_BODY: usize = 1024 * 1024;
const MAX_TEXT: usize = 64 * 1024;
const MAX_EVENTS: usize = 100_000;
const PAGE_SIZE: usize = 100;
const MULTIPART_WINDOW_MS: u64 = 120_000;

#[derive(Clone)]
pub(crate) struct Store(crate::sms_store_health::GuardedArchive);

#[derive(Clone, Default, Deserialize, Serialize)]
struct Snapshot {
    #[serde(default = "schema_version")]
    schema_version: u16,
    revision: u64,
    #[serde(default)]
    profiles: BTreeMap<String, Value>,
    #[serde(default)]
    desired: BTreeMap<String, Desired>,
    #[serde(default)]
    messages: BTreeMap<String, Value>,
    #[serde(default)]
    events: BTreeMap<String, Event>,
    #[serde(default)]
    requests: BTreeMap<String, Event>,
    #[serde(default)]
    send_intents: BTreeMap<String, SendIntent>,
    #[serde(default)]
    inbound_dedup: BTreeMap<String, Event>,
    #[serde(default)]
    changes: Vec<Value>,
}
fn schema_version() -> u16 {
    1
}

#[derive(Clone, Deserialize, Serialize)]
struct Desired {
    desired_state: String,
    revision: u64,
}
#[derive(Clone, Deserialize, Serialize)]
struct Event {
    hash: String,
    response: Value,
}
#[derive(Clone, Deserialize, Serialize)]
struct SendIntent {
    profile_id: String,
    client_request_id: String,
    phone: String,
    body: String,
    hash: String,
    phase: String,
    created_at_ms: u64,
}

fn failure(code: &str) -> StoreError {
    StoreError::Configuration(code.to_owned())
}
fn invalid() -> StoreError {
    failure("invalid_request")
}
fn conflict() -> StoreError {
    failure("state_conflict")
}
fn id(value: &Value, key: &str) -> Result<String, StoreError> {
    let text = value.get(key).and_then(Value::as_str).ok_or_else(invalid)?;
    if text.is_empty()
        || text.len() > 128
        || text.trim() != text
        || text.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    Ok(text.to_owned())
}
fn text(value: &Value, key: &str, limit: usize) -> Result<String, StoreError> {
    let text = value.get(key).and_then(Value::as_str).ok_or_else(invalid)?;
    if text.len() > limit {
        return Err(invalid());
    }
    Ok(text.to_owned())
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
fn digest(value: &Value) -> Result<String, StoreError> {
    Ok(stable_digest_hex(
        &serde_json::to_vec(value).map_err(StoreError::Json)?,
    ))
}
fn scope(profile: &str, item: &str) -> String {
    format!("{profile}\0{item}")
}
// Preserve the historic Go identifier bytes while making Rust the sole owner
// of physical card/Profile resource identity: SHA-256(kind NUL parts), first
// 16 bytes, lowercase hex.
fn multi_resource_id(kind: &str, parts: &[&str]) -> String {
    let mut input = Vec::new();
    input.extend_from_slice(kind.as_bytes());
    for part in parts {
        input.push(0);
        input.extend_from_slice(part.as_bytes());
    }
    format!(
        "{kind}_{}",
        stable_digest_hex(&input)
            .chars()
            .take(32)
            .collect::<String>()
    )
}
fn normalize_catalog_iccid(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_end_matches(['F', 'f'])
        .to_owned()
}
fn conversation_key(profile: &str, peer: &str) -> String {
    stable_digest_hex(scope(profile, peer).as_bytes())
}
fn visible_message(message: &Value) -> bool {
    matches!(message["direction"].as_str(), Some("inbound" | "outbound"))
        && !matches!(message["state"].as_str(), Some("partial" | "ambiguous"))
        && message["deleted"] != true
}
fn public_message(mut message: Value) -> Value {
    if let Some(map) = message.as_object_mut() {
        for key in [
            "parts",
            "attempt_token",
            "auto_group",
            "group_first_at_ms",
            "group_last_at_ms",
            "ambiguity_reason",
        ] {
            map.remove(key);
        }
    }
    message
}
fn chronological_key(message: &Value) -> (u64, String) {
    (
        message["created_at_ms"].as_u64().unwrap_or(0),
        message["message_id"].as_str().unwrap_or("").to_owned(),
    )
}
#[derive(Serialize, Deserialize)]
struct HistoryCursor {
    profile: String,
    peer: Option<String>,
    at_ms: u64,
    message_id: String,
}
#[derive(Serialize, Deserialize)]
struct ConversationCursor {
    profile: Option<String>,
    key: String,
}
fn encode_cursor<T: Serialize>(cursor: &T) -> Result<String, StoreError> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor).map_err(StoreError::Json)?))
}
fn decode_cursor<T: for<'de> Deserialize<'de>>(cursor: &str) -> Result<T, StoreError> {
    if cursor.is_empty() || cursor.len() > 4096 {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| invalid())?;
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}
fn decode_peer(value: &str) -> Result<String, StoreError> {
    if value.len() > 512 || value.len() % 2 != 0 {
        return Err(invalid());
    }
    let bytes = value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(|_| invalid())?;
            u8::from_str_radix(text, 16).map_err(|_| invalid())
        })
        .collect::<Result<Vec<_>, _>>()?;
    String::from_utf8(bytes).map_err(|_| invalid())
}
fn read_snapshot(items: &[Value]) -> Result<Snapshot, StoreError> {
    if items.is_empty() {
        return Ok(Snapshot {
            schema_version: 1,
            ..Default::default()
        });
    }
    if items.len() != 1 {
        return Err(failure("invalid_store_schema"));
    }
    let state: Snapshot = serde_json::from_value(items[0].clone()).map_err(StoreError::Json)?;
    if state.schema_version != 1 {
        return Err(failure("invalid_store_schema"));
    }
    Ok(state)
}
fn persist_snapshot(items: &mut Vec<Value>, state: Snapshot) -> Result<(), StoreError> {
    *items = vec![serde_json::to_value(state).map_err(StoreError::Json)?];
    Ok(())
}
fn next_revision(state: &mut Snapshot) -> Result<u64, StoreError> {
    state.revision = state
        .revision
        .checked_add(1)
        .ok_or_else(|| failure("store_capacity"))?;
    Ok(state.revision)
}
fn change(state: &mut Snapshot, profile: &str, message: &str) {
    let entry = state.messages.get(&scope(profile, message));
    let conversation = entry.and_then(|m| {
        m["conversation_id"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                m["peer"]
                    .as_str()
                    .map(|peer| conversation_key(profile, peer))
            })
    });
    state.changes.push(json!({"revision":state.revision,"profile_id":profile,"message_id":message,"direction":entry.map(|m|m["direction"].clone()),"kind":"message","state":entry.map(|m|m["state"].clone()),"deleted":entry.map(|m|m["deleted"].clone()),"conversation_id":conversation}));
}
fn require_profile(state: &Snapshot, profile: &str, live: bool) -> Result<(), StoreError> {
    let catalog = state
        .profiles
        .get(profile)
        .ok_or_else(|| failure("profile_not_found"))?;
    if live && catalog["tombstone"].as_bool().unwrap_or(false) {
        return Err(failure("profile_deleted"));
    }
    Ok(())
}

fn send_intent_result(
    state: &Snapshot,
    profile: &str,
    request: &str,
    can_submit: bool,
) -> Result<Value, StoreError> {
    let key = scope(profile, request);
    if let Some(prior) = state.requests.get(&key) {
        let message_id = prior.response["message_id"].as_str().ok_or_else(conflict)?;
        let message = state
            .messages
            .get(&scope(profile, message_id))
            .ok_or_else(conflict)?;
        return Ok(
            json!({"profile_id":profile,"client_request_id":request,"phone":message["peer"].as_str().unwrap_or(""),"body":message["body"].as_str().unwrap_or(""),
            "phase":"queued","state":if message["deleted"]==true { json!("cancelled") } else {message["state"].clone()},
            "message_id":message_id,"revision":message["revision"],"created_at_ms":message["created_at_ms"],"can_submit":false}),
        );
    }
    let intent = state
        .send_intents
        .get(&key)
        .ok_or_else(|| failure("send_intent_not_found"))?;
    Ok(
        json!({"profile_id":profile,"client_request_id":request,"phone":intent.phone,"body":intent.body,"phase":intent.phase,
        "state":if intent.phase=="prepared" {"prepared"} else {"unknown"},"created_at_ms":intent.created_at_ms,"can_submit":can_submit}),
    )
}

impl Store {
    pub(crate) fn open(root: &Path) -> Result<Self, StoreError> {
        Ok(Self(
            ArchiveStore::with_limits(
                root.join("multistandby").join("store-v1.json"),
                64 * 1024 * 1024,
                1,
            )?
            .into(),
        ))
    }
    pub(crate) fn status(&self) -> proletarius_api_contract::SmsStoreStatus {
        self.0.status()
    }

    /// Read the durable profile catalog for native N-wait runtime reporting.
    /// The returned projection is read-only and includes the existing card
    /// association used by the Go report route's fallback lookup.
    pub(crate) fn runtime_catalog(&self) -> Result<Value, StoreError> {
        self.read("catalog", None, 0)
    }

    /// Select durable restart intent against the profiles physically observed
    /// in the current inventory pass. Go retains device/session execution;
    /// Rust owns the persisted recovery policy.
    pub(crate) fn recovery_selection(
        &self,
        live_profile_ids: &[String],
    ) -> Result<Value, StoreError> {
        self.0.read(|items| {
            let state = read_snapshot(items)?;
            let live: HashSet<&str> = live_profile_ids.iter().map(String::as_str).collect();
            let candidates: Vec<_> = state
                .desired
                .iter()
                .filter(|(profile_id, desired)| {
                    desired.desired_state == "keep_online" && live.contains(profile_id.as_str())
                })
                .filter_map(|(profile_id, _)| {
                    state.profiles.get(profile_id).and_then(|profile| {
                        (!profile["tombstone"].as_bool().unwrap_or(false))
                            .then(|| (profile_id, profile["card_id"].as_str().unwrap_or_default()))
                    })
                })
                .collect();
            let chosen_card = candidates
                .first()
                .map(|(_, card_id)| *card_id)
                .unwrap_or("");
            let profile_ids: Vec<_> = candidates
                .iter()
                .filter(|(_, card_id)| *card_id == chosen_card)
                .map(|(profile_id, _)| (*profile_id).clone())
                .collect();
            Ok(json!({"profile_ids":profile_ids,"card_id":chosen_card,"revision":state.revision}))
        })
    }

    pub(crate) fn data_root(&self) -> &Path {
        self.0
            .path()
            .parent()
            .and_then(Path::parent)
            .expect("fixed store path")
    }
    pub(crate) fn snapshot_file(&self) -> Result<Option<std::fs::File>, StoreError> {
        self.0.snapshot_file()
    }
    pub(crate) fn snapshot_failed(&self, error: StoreError) -> StoreError {
        self.0.snapshot_failed(error)
    }

    /// A previous process may have crossed the network boundary after claiming.
    /// Never automatically resubmit these intents following process restart.
    pub(crate) fn recover(&self) -> Result<(), StoreError> {
        self.0.mutate(|items| {
            let mut state = read_snapshot(items)?;
            let keys: Vec<_> = state
                .messages
                .iter()
                .filter(|(_, m)| m["state"] == "submitting")
                .map(|(key, _)| key.clone())
                .collect();
            for key in keys {
                let rev = next_revision(&mut state)?;
                let message = state.messages.get_mut(&key).expect("collected message");
                message["state"] = json!("unknown");
                message["revision"] = json!(rev);
                message["error"] = json!("backend_restarted_during_submission");
                let profile = message["profile_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let message_id = message["message_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                change(&mut state, &profile, &message_id);
            }
            persist_snapshot(items, state)
        })
    }

    /// Native send preparation lives in the existing private, atomically
    /// replaced archive. It does not enqueue or touch a card. Only a confirmed
    /// first transition to submitting grants the native caller permission to
    /// invoke the legacy queue endpoint; recovery and repeated calls cannot.
    fn prepare_send_intent(&self, body: Value, submit: bool) -> Result<Value, StoreError> {
        let profile = id(&body, "profile_id")?;
        let request = id(&body, "client_request_id")?;
        let phone = text(&body, "phone", 256)?;
        let content = text(&body, "body", MAX_TEXT)?;
        if phone.trim().is_empty() || content.trim().is_empty() {
            return Err(invalid());
        }
        let hash = digest(&json!({"phone":phone,"body":content}))?;
        let key = scope(&profile, &request);
        self.0.mutate(|items| {
            let mut state = read_snapshot(items)?;
            require_profile(&state, &profile, true)?;
            if let Some(prior) = state.requests.get(&key) {
                if prior.hash != hash {
                    return Err(conflict());
                }
                return send_intent_result(&state, &profile, &request, false);
            }
            let can_submit = if let Some(intent) = state.send_intents.get_mut(&key) {
                if intent.hash != hash {
                    return Err(conflict());
                }
                if submit && intent.phase == "prepared" {
                    intent.phase = "submitting".to_owned();
                    true
                } else {
                    false
                }
            } else if submit {
                return Err(failure("send_intent_not_found"));
            } else {
                state.send_intents.insert(
                    key.clone(),
                    SendIntent {
                        profile_id: profile.clone(),
                        client_request_id: request.clone(),
                        phone,
                        body: content,
                        hash,
                        phase: "prepared".to_owned(),
                        created_at_ms: now_ms(),
                    },
                );
                false
            };
            let result = send_intent_result(&state, &profile, &request, can_submit)?;
            persist_snapshot(items, state)?;
            Ok(result)
        })
    }

    fn send_intents(
        &self,
        profile: Option<&str>,
        request: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<Value, StoreError> {
        let state = self.0.read(read_snapshot)?;
        if let Some(request) = request {
            if cursor.is_some() {
                return Err(invalid());
            }
            return send_intent_result(&state, profile.ok_or_else(invalid)?, request, false);
        }
        if profile.is_some() {
            return Err(invalid());
        }
        let after = cursor.map(decode_cursor::<String>).transpose()?;
        let mut pending = BTreeMap::new();
        for (key, intent) in &state.send_intents {
            pending.insert(
                key.clone(),
                send_intent_result(&state, &intent.profile_id, &intent.client_request_id, false)?,
            );
        }
        // A process interrupted after an actual claim has durable unknown
        // evidence in its original message, even after the preparation record
        // was atomically consumed by queue. Legacy messages remain readable.
        for message in state.messages.values().filter(|m| {
            m["direction"] == "outbound" && m["state"] == "unknown" && m["deleted"] != true
        }) {
            if let (Some(profile), Some(request)) = (
                message["profile_id"].as_str(),
                message["client_request_id"].as_str(),
            ) {
                if state.requests.contains_key(&scope(profile, request)) {
                    pending.insert(
                        scope(profile, request),
                        send_intent_result(&state, profile, request, false)?,
                    );
                }
            }
        }
        let page: Vec<_> = pending
            .into_iter()
            .filter(|(key, _)| after.as_ref().is_none_or(|after| key > after))
            .take(PAGE_SIZE + 1)
            .collect();
        let more = page.len() > PAGE_SIZE;
        let next = if more {
            Some(encode_cursor(&page[PAGE_SIZE - 1].0)?)
        } else {
            None
        };
        Ok(
            json!({"intents":page.into_iter().take(PAGE_SIZE).map(|(_, value)|value).collect::<Vec<_>>(),"next_cursor":next}),
        )
    }

    pub(crate) fn apply(&self, action: &str, body: Value) -> Result<Value, StoreError> {
        let event_id = id(&body, "event_id")?;
        let hash = digest(&json!({"action":action,"body":body}))?;
        self.0.mutate(|items| {
            let mut state = read_snapshot(items)?;
            if let Some(prior) = state.events.get(&event_id) {
                if prior.hash != hash {
                    return Err(conflict());
                }
                // Cached claim success cannot authorize a new network submission
                // after process recovery or after the desired-state fence moved.
                if matches!(action, "claim" | "claim-next") {
                    let profile = id(&body, "profile_id")?;
                    let attempt = id(&body, "attempt_token")?;
                    let message = if action == "claim-next" {
                        prior.response["message"]["message_id"]
                            .as_str()
                            .ok_or_else(conflict)?
                            .to_owned()
                    } else {
                        id(&body, "message_id")?
                    };
                    let current = state
                        .messages
                        .get(&scope(&profile, &message))
                        .ok_or_else(conflict)?;
                    let desired = state.desired.get(&profile).ok_or_else(conflict)?;
                    if current["state"] != "submitting"
                        || current["attempt_token"] != attempt
                        || desired.desired_state != "keep_online"
                        || (action == "claim-next"
                            && prior.response["desired_revision"].as_u64()
                                != Some(desired.revision))
                        || body
                            .get("expected_revision")
                            .is_some_and(|v| v.as_u64() != Some(desired.revision))
                    {
                        return Err(conflict());
                    }
                }
                return Ok(prior.response.clone());
            }
            if state.events.len() >= MAX_EVENTS {
                return Err(failure("store_capacity"));
            }
            let response = self.apply_new(&mut state, action, &body)?;
            // An empty outbox poll is read-only. Do not consume durable event
            // capacity on the one-second maintenance cadence when there is
            // nothing to send.
            let empty_claim = action == "claim-next" && response["message"].is_null();
            if !empty_claim {
                state.events.insert(
                    event_id,
                    Event {
                        hash,
                        response: response.clone(),
                    },
                );
            } else {
                return Ok(response);
            }
            persist_snapshot(items, state)?;
            Ok(response)
        })
    }

    fn apply_new(
        &self,
        state: &mut Snapshot,
        action: &str,
        body: &Value,
    ) -> Result<Value, StoreError> {
        if action == "catalog" && body.get("reader").is_some() {
            let object = body.as_object().ok_or_else(invalid)?;
            if object.len() != 3
                || object
                    .keys()
                    .any(|key| !matches!(key.as_str(), "event_id" | "reader" | "profiles"))
            {
                return Err(invalid());
            }
            let reader = id(body, "reader")?;
            let source = body["profiles"].as_array().ok_or_else(invalid)?;
            if source.len() > 4096 {
                return Err(invalid());
            }
            let mut identities = Vec::with_capacity(source.len());
            let mut unique = HashSet::with_capacity(source.len());
            let mut normalized_iccids = HashSet::with_capacity(source.len());
            for profile in source {
                let fields = profile.as_object().ok_or_else(invalid)?;
                if fields.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "eid" | "iccid" | "display_name" | "phone_number"
                    )
                }) {
                    return Err(invalid());
                }
                let eid = text(profile, "eid", 128)?;
                let iccid = text(profile, "iccid", 128)?;
                if eid.is_empty()
                    || iccid.is_empty()
                    || eid.trim() != eid
                    || iccid.trim() != iccid
                    || eid.chars().chain(iccid.chars()).any(char::is_control)
                {
                    return Err(invalid());
                }
                let profile_id = multi_resource_id("profile", &[&eid, &iccid]);
                let card_id = multi_resource_id("card", &[&reader, &eid]);
                if !unique.insert(profile_id.clone())
                    || !normalized_iccids.insert((eid.clone(), normalize_catalog_iccid(&iccid)))
                {
                    return Err(invalid());
                }
                let mut catalog_profile = json!({
                    "profile_id": profile_id,
                    "card_id": card_id,
                    "eid": eid,
                    "iccid": iccid,
                    "tombstone": false
                });
                for key in ["display_name", "phone_number"] {
                    if profile.get(key).is_some() {
                        catalog_profile[key] = json!(text(profile, key, 512)?);
                    }
                }
                identities.push(catalog_profile);
            }
            for profile in &identities {
                let profile_id = id(profile, "profile_id")?;
                state.profiles.insert(profile_id, profile.clone());
            }
            let revision = next_revision(state)?;
            return Ok(json!({"revision":revision,"profiles":identities}));
        }
        if action == "catalog" {
            let profiles = body["profiles"].as_array().ok_or_else(invalid)?;
            if profiles.len() > 4096 {
                return Err(invalid());
            }
            for profile in profiles {
                let profile_id = id(profile, "profile_id")?;
                id(profile, "card_id")?;
                // Only explicit catalog fields may enter the store. Reject secrets
                // accidentally embedded in a live runtime object.
                let allowed = [
                    "profile_id",
                    "card_id",
                    "eid",
                    "iccid",
                    "display_name",
                    "phone_number",
                    "tombstone",
                ];
                let object = profile.as_object().ok_or_else(invalid)?;
                if object.keys().any(|k| !allowed.contains(&k.as_str())) {
                    return Err(invalid());
                }
                for key in ["eid", "iccid", "display_name", "phone_number"] {
                    if profile.get(key).is_some() {
                        text(profile, key, 512)?;
                    }
                }
                state.profiles.insert(profile_id, profile.clone());
            }
            let revision = next_revision(state)?;
            return Ok(json!({"revision":revision,"updated":profiles.len()}));
        }
        let profile = id(body, "profile_id")?;
        require_profile(
            state,
            &profile,
            matches!(action, "desired" | "queue" | "claim" | "claim-next"),
        )?;
        match action {
            "desired" => {
                let desired = text(body, "desired_state", 32)?;
                if !matches!(desired.as_str(), "keep_online" | "stopped") {
                    return Err(invalid());
                }
                let revision = next_revision(state)?;
                state.desired.insert(
                    profile.clone(),
                    Desired {
                        desired_state: desired.clone(),
                        revision,
                    },
                );
                Ok(json!({"profile_id":profile,"desired_state":desired,"revision":revision}))
            }
            "queue" => {
                let request = id(body, "client_request_id")?;
                let phone = text(body, "phone", 256)?;
                let content = text(body, "body", MAX_TEXT)?;
                if phone.trim().is_empty() || content.is_empty() {
                    return Err(invalid());
                }
                let payload_hash = digest(&json!({"phone":phone,"body":content}))?;
                let request_key = scope(&profile, &request);
                if state
                    .send_intents
                    .get(&request_key)
                    .is_some_and(|intent| intent.hash != payload_hash)
                {
                    return Err(conflict());
                }
                if let Some(prior) = state.requests.get(&request_key) {
                    return if prior.hash == payload_hash {
                        Ok(prior.response.clone())
                    } else {
                        Err(conflict())
                    };
                }
                if state
                    .desired
                    .get(&profile)
                    .map(|d| d.desired_state.as_str())
                    != Some("keep_online")
                {
                    return Err(failure("profile_stopped"));
                }
                let message_id = format!("msg-{}", random_token()?);
                let revision = next_revision(state)?;
                let message = json!({"message_id":message_id,"profile_id":profile,"direction":"outbound","peer":phone,"phone":phone,"body":content,"created_at_ms":now_ms(),"state":"queued","client_request_id":request,"revision":revision,"deleted":false});
                state.messages.insert(scope(&profile, &message_id), message);
                let response =
                    json!({"message_id":message_id,"state":"queued","revision":revision});
                state.requests.insert(
                    request_key.clone(),
                    Event {
                        hash: payload_hash,
                        response: response.clone(),
                    },
                );
                state.send_intents.remove(&request_key);
                change(state, &profile, &message_id);
                Ok(response)
            }
            "claim" => {
                let message_id = id(body, "message_id")?;
                let attempt = id(body, "attempt_token")?;
                let desired = state
                    .desired
                    .get(&profile)
                    .ok_or_else(|| failure("profile_stopped"))?;
                if desired.desired_state != "keep_online" {
                    return Err(failure("profile_stopped"));
                }
                if let Some(expected) = body.get("expected_revision")
                    && expected.as_u64() != Some(desired.revision)
                {
                    return Err(conflict());
                }
                let key = scope(&profile, &message_id);
                let message = state
                    .messages
                    .get(&key)
                    .ok_or_else(|| failure("message_not_found"))?;
                if message["direction"] != "outbound" || message["deleted"] == true {
                    return Err(conflict());
                }
                if message["state"] == "submitting" && message["attempt_token"] == attempt {
                    return Ok(
                        json!({"message_id":message_id,"state":"submitting","revision":message["revision"]}),
                    );
                }
                if message["state"] != "queued" {
                    return Err(conflict());
                }
                let revision = next_revision(state)?;
                let message = state.messages.get_mut(&key).expect("validated message");
                message["state"] = json!("submitting");
                message["attempt_token"] = json!(attempt);
                message["revision"] = json!(revision);
                change(state, &profile, &message_id);
                Ok(json!({"message_id":message_id,"state":"submitting","revision":revision}))
            }
            "claim-next" => {
                let Some(object) = body.as_object() else {
                    return Err(invalid());
                };
                if object.len() != 3
                    || object.keys().any(|key| {
                        !matches!(key.as_str(), "event_id" | "profile_id" | "attempt_token")
                    })
                {
                    return Err(invalid());
                }
                let attempt = id(body, "attempt_token")?;
                let desired = state
                    .desired
                    .get(&profile)
                    .ok_or_else(|| failure("profile_stopped"))?;
                if desired.desired_state != "keep_online" {
                    return Err(failure("profile_stopped"));
                }
                let desired_revision = desired.revision;
                // `messages` is a BTreeMap keyed by profile + NUL + message ID.
                // Selecting its first queued item preserves the existing
                // pending-list order while making selection and claim atomic.
                let key = state
                    .messages
                    .iter()
                    .find(|(_, message)| {
                        message["profile_id"] == profile
                            && message["direction"] == "outbound"
                            && message["state"] == "queued"
                            && message["deleted"] != true
                    })
                    .map(|(key, _)| key.clone());
                let Some(key) = key else {
                    return Ok(json!({"message":null}));
                };
                let message_id = state.messages[&key]["message_id"]
                    .as_str()
                    .ok_or_else(invalid)?
                    .to_owned();
                let revision = next_revision(state)?;
                let message = state.messages.get_mut(&key).expect("selected message");
                message["state"] = json!("submitting");
                message["attempt_token"] = json!(attempt);
                message["revision"] = json!(revision);
                let claimed = message.clone();
                change(state, &profile, &message_id);
                Ok(json!({"message":claimed,"desired_revision":desired_revision}))
            }
            "outcome" => {
                let message_id = id(body, "message_id")?;
                let attempt = id(body, "attempt_token")?;
                let outcome = text(body, "state", 32)?;
                if !matches!(
                    outcome.as_str(),
                    "accepted" | "delivered" | "failed" | "unknown"
                ) {
                    return Err(invalid());
                }
                let key = scope(&profile, &message_id);
                let message = state
                    .messages
                    .get(&key)
                    .ok_or_else(|| failure("message_not_found"))?;
                if message["direction"] != "outbound" || message["attempt_token"] != attempt {
                    return Err(conflict());
                }
                let current = message["state"].as_str().unwrap_or_default();
                if current == "queued"
                    || (matches!(current, "delivered" | "failed") && current != outcome)
                    || (current == "accepted" && outcome == "unknown")
                {
                    return Err(conflict());
                }
                let revision = next_revision(state)?;
                let message = state.messages.get_mut(&key).expect("validated message");
                message["state"] = json!(outcome);
                message["revision"] = json!(revision);
                for field in ["sip_code", "rp_cause", "error"] {
                    if let Some(value) = body.get(field) {
                        message[field] = value.clone();
                    }
                }
                change(state, &profile, &message_id);
                Ok(json!({"message_id":message_id,"state":outcome,"revision":revision}))
            }
            "inbound" => self.inbound(state, &profile, body),
            "inbound-event" => self.inbound_event(state, &profile, body),
            _ => Err(failure("route_not_found")),
        }
    }

    fn inbound_event(
        &self,
        state: &mut Snapshot,
        profile: &str,
        event: &Value,
    ) -> Result<Value, StoreError> {
        let event_id = id(event, "event_id")?;
        let rpdu = text(event, "raw_rpdu_hex", MAX_TEXT)?;
        let tpdu = text(event, "raw_tpdu_hex", MAX_TEXT)?;
        let digest = stable_digest_hex(format!("mt\0{profile}\0{rpdu}").as_bytes());
        // Keep the existing Go multiID wire identity: SHA-256 over the
        // NUL-delimited kind/profile/raw-RPDU tuple, truncated to 128 bits.
        let dedup = format!("mt_{}", &digest[..32]);
        let timestamp = event
            .get("timestamp_ms")
            .and_then(Value::as_i64)
            .ok_or_else(invalid)?;
        let timestamp_missing = event
            .get("timestamp_missing")
            .and_then(Value::as_bool)
            .ok_or_else(invalid)?;
        let received_at = if timestamp_missing {
            now_ms()
        } else if timestamp >= 0 {
            timestamp as u64
        } else {
            return Err(invalid());
        };
        let decoded = event
            .get("decoded")
            .and_then(Value::as_bool)
            .ok_or_else(invalid)?;
        let status_report = event
            .get("status_report")
            .and_then(Value::as_bool)
            .ok_or_else(invalid)?;
        let kind = if status_report {
            "status_report"
        } else if decoded {
            "message"
        } else {
            "undecoded"
        };
        let mut inbound = json!({
            "event_id":event_id,
            "profile_id":profile,
            "message_id":dedup,
            "sender":text(event,"sender",256)?,
            "body":text(event,"content",MAX_TEXT)?,
            "received_at_ms":received_at,
            "raw_rpdu_hex":rpdu,
            "raw_tpdu_hex":tpdu,
            "dedup_key":dedup,
            "kind":kind,
        });
        if let Some(concat) = event.get("concat") {
            let reference = concat
                .get("reference")
                .and_then(Value::as_i64)
                .ok_or_else(invalid)?;
            let reference_bits = concat
                .get("reference_bits")
                .and_then(Value::as_i64)
                .ok_or_else(invalid)?;
            let sequence = concat
                .get("sequence")
                .and_then(Value::as_i64)
                .ok_or_else(invalid)?;
            let total = concat
                .get("total")
                .and_then(Value::as_i64)
                .ok_or_else(invalid)?;
            inbound["part"] = json!({
                "reference":format!("{reference_bits}:{reference}"),
                "index":sequence,
                "total":total,
                "auto_group":true,
            });
        }
        if status_report {
            inbound["report"] = json!({
                "status":text(event,"delivery_status",256)?,
                "status_code":event.get("status_code").cloned().unwrap_or(Value::Null),
                "message_reference":event.get("message_reference").cloned().unwrap_or(Value::Null),
            });
        }

        let has_concat = event.get("concat").is_some();
        let before = has_concat.then(|| state.clone());
        match self.inbound(state, profile, &inbound) {
            Ok(value) => Ok(value),
            Err(_) if has_concat => {
                *state = before.expect("multipart state checkpoint exists");
                inbound.as_object_mut().ok_or_else(invalid)?.remove("part");
                inbound["kind"] = json!("undecoded");
                inbound["dedup_key"] = json!(format!("{dedup}_raw"));
                self.inbound(state, profile, &inbound)
            }
            Err(error) => Err(error),
        }
    }

    fn inbound(
        &self,
        state: &mut Snapshot,
        profile: &str,
        body: &Value,
    ) -> Result<Value, StoreError> {
        let message_id = id(body, "message_id")?;
        let dedup = id(body, "dedup_key")?;
        let key = scope(profile, &dedup);
        let mut payload = body.clone();
        payload
            .as_object_mut()
            .ok_or_else(invalid)?
            .remove("event_id");
        payload
            .as_object_mut()
            .ok_or_else(invalid)?
            .remove("received_at_ms");
        let hash = digest(&payload)?;
        if let Some(prior) = state.inbound_dedup.get(&key) {
            return if prior.hash == hash {
                Ok(prior.response.clone())
            } else {
                Err(conflict())
            };
        }
        let kind = body
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("message");
        if !matches!(kind, "message" | "status_report" | "undecoded") {
            return Err(invalid());
        }
        for field in ["raw_rpdu_hex", "raw_tpdu_hex"] {
            if let Some(value) = body.get(field) {
                let hex = value.as_str().ok_or_else(invalid)?;
                if hex.len() > MAX_TEXT
                    || hex.len() % 2 != 0
                    || !hex.bytes().all(|c| c.is_ascii_hexdigit())
                {
                    return Err(invalid());
                }
            }
        }
        let revision = next_revision(state)?;
        let response = if matches!(kind, "status_report" | "undecoded") {
            if kind == "undecoded"
                && body.get("raw_rpdu_hex").is_none()
                && body.get("raw_tpdu_hex").is_none()
            {
                return Err(invalid());
            }
            let record_state = if kind == "undecoded" {
                "undecoded"
            } else {
                "unmatched"
            };
            // Durable unmatched reports are not user messages and never guess by
            // an eight-bit TP-MR. A later explicit attempt-bound outcome may link it.
            let report_key = scope(profile, &format!("report:{dedup}"));
            state.messages.insert(report_key,json!({"message_id":message_id,"profile_id":profile,"direction":"report","kind":kind,"state":record_state,"created_at_ms":now_ms(),"revision":revision,"report":body.get("report").cloned().unwrap_or(Value::Null),"raw_rpdu_hex":body.get("raw_rpdu_hex"),"raw_tpdu_hex":body.get("raw_tpdu_hex")}));
            json!({"message_id":message_id,"state":record_state,"revision":revision,"complete":false})
        } else {
            Self::inbound_message(state, profile, body, &message_id, &dedup, revision)?
        };
        state.inbound_dedup.insert(
            key,
            Event {
                hash,
                response: response.clone(),
            },
        );
        Ok(response)
    }

    /// Auto grouping is chosen inside the same durable transaction as the raw
    /// fragment and network dedup record. A completed group never absorbs a new
    /// reference reuse; ambiguous incomplete groups quarantine later fragments.
    fn inbound_message(
        state: &mut Snapshot,
        profile: &str,
        body: &Value,
        incoming_id: &str,
        dedup: &str,
        revision: u64,
    ) -> Result<Value, StoreError> {
        let sender = text(body, "sender", 256)?;
        let content = text(body, "body", MAX_TEXT)?;
        let received = body["received_at_ms"].as_u64().ok_or_else(invalid)?;
        let (index, total, reference, auto_group) = if let Some(part) = body.get("part") {
            let index = part["index"].as_u64().ok_or_else(invalid)?;
            let total = part["total"].as_u64().ok_or_else(invalid)?;
            if total == 0 || total > 255 || index == 0 || index > total {
                return Err(invalid());
            }
            (
                index,
                total,
                text(part, "reference", 128)?,
                part["auto_group"] == true && total > 1,
            )
        } else {
            (1, 1, String::new(), false)
        };
        let part = json!({"body":content,"raw_rpdu_hex":body.get("raw_rpdu_hex"),"raw_tpdu_hex":body.get("raw_tpdu_hex")});
        let index_key = index.to_string();
        let mut message_id = incoming_id.to_owned();
        if auto_group {
            // The reference includes its width (8/16 bit). Compare the complete
            // observed timestamp span so a chain of late fragments cannot extend
            // one group indefinitely beyond the supported correlation window.
            let candidates: Vec<String> = state
                .messages
                .iter()
                .filter(|(_, message)| {
                    let first = message["group_first_at_ms"].as_u64().unwrap_or(0);
                    let last = message["group_last_at_ms"].as_u64().unwrap_or(first);
                    message["profile_id"] == profile
                        && message["peer"] == sender
                        && message["auto_group"] == true
                        && message["parts_total"] == total
                        && message["part_reference"] == reference
                        && message["deleted"] != true
                        && matches!(message["state"].as_str(), Some("partial" | "ambiguous"))
                        && last.max(received).saturating_sub(first.min(received))
                            <= MULTIPART_WINDOW_MS
                })
                .map(|(key, _)| key.clone())
                .collect();
            let ambiguous = candidates.len() > 1
                || candidates.first().is_some_and(|key| {
                    let message = &state.messages[key];
                    message["state"] == "ambiguous"
                        || message["parts"]
                            .get(&index_key)
                            .is_some_and(|old| old != &part)
                });
            if ambiguous {
                for key in &candidates {
                    let message = state.messages.get_mut(key).expect("candidate exists");
                    message["state"] = json!("ambiguous");
                    message["revision"] = json!(revision);
                    message["ambiguity_reason"] = json!("multipart_reference_collision");
                }
                // Preserve this transaction independently. Existing candidate
                // parts stay durable, but neither source is a public message.
                let raw_key = scope(profile, &format!("undecoded:{dedup}"));
                state.messages.insert(raw_key, json!({"message_id":incoming_id,"profile_id":profile,
                    "direction":"report","kind":"undecoded","state":"undecoded","peer":sender,
                    "body":content,"created_at_ms":received,"revision":revision,
                    "reason":"multipart_group_ambiguous","part":body.get("part"),
                    "raw_rpdu_hex":body.get("raw_rpdu_hex"),"raw_tpdu_hex":body.get("raw_tpdu_hex")}));
                return Ok(
                    json!({"message_id":incoming_id,"state":"undecoded","complete":false,"revision":revision}),
                );
            }
            if let Some(key) = candidates.first() {
                message_id = state.messages[key]["message_id"]
                    .as_str()
                    .ok_or_else(invalid)?
                    .to_owned();
            } else {
                message_id = format!("concat_{}", digest(&json!([profile, dedup]))?);
            }
        }
        let message_key = scope(profile, &message_id);
        let message = state.messages.entry(message_key).or_insert_with(|| json!({"message_id":message_id,
            "profile_id":profile,"direction":"inbound","peer":sender,"body":"","created_at_ms":received,
            "state":"partial","parts":{},"parts_total":total,"part_reference":reference,"deleted":false,
            "auto_group":auto_group,"group_first_at_ms":received,"group_last_at_ms":received}));
        if message["direction"] != "inbound"
            || message["peer"] != sender
            || message["parts_total"] != total
            || message["part_reference"] != reference
        {
            return Err(conflict());
        }
        let parts = message["parts"].as_object_mut().ok_or_else(invalid)?;
        if let Some(old) = parts.get(&index_key)
            && old != &part
        {
            return Err(conflict());
        }
        parts.insert(index_key, part);
        let complete = parts.len() == total as usize;
        let assembled = if complete {
            (1..=total)
                .map(|i| parts[&i.to_string()]["body"].as_str().unwrap_or_default())
                .collect::<String>()
        } else {
            String::new()
        };
        if assembled.len() > MAX_TEXT {
            return Err(invalid());
        }
        let was_complete = message["state"] == "received";
        message["body"] = json!(assembled);
        message["state"] = json!(if complete { "received" } else { "partial" });
        message["revision"] = json!(revision);
        message["group_first_at_ms"] = json!(
            message["group_first_at_ms"]
                .as_u64()
                .unwrap_or(received)
                .min(received)
        );
        message["group_last_at_ms"] = json!(
            message["group_last_at_ms"]
                .as_u64()
                .unwrap_or(received)
                .max(received)
        );
        if complete && !was_complete && message["deleted"] != true {
            change(state, profile, &message_id);
        }
        Ok(
            json!({"message_id":message_id,"state":if complete {"received"} else {"partial"},"complete":complete,"revision":revision}),
        )
    }

    /// Chronological history is separate from the revision feed. A cursor is
    /// bound to its profile/conversation and remains valid after its anchor is deleted.
    fn history(
        &self,
        profile: &str,
        peer: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<Value, StoreError> {
        let state = self.0.read(read_snapshot)?;
        require_profile(&state, profile, false)?;
        let before = cursor.map(decode_cursor::<HistoryCursor>).transpose()?;
        if before
            .as_ref()
            .is_some_and(|c| c.profile != profile || c.peer.as_deref() != peer)
        {
            return Err(invalid());
        }
        let anchor = before.as_ref().map(|c| (c.at_ms, c.message_id.clone()));
        let mut messages: Vec<_> = state
            .messages
            .values()
            .filter(|m| {
                visible_message(m)
                    && m["profile_id"] == profile
                    && peer.is_none_or(|peer| m["peer"] == peer)
                    && anchor
                        .as_ref()
                        .is_none_or(|anchor| chronological_key(m) < *anchor)
            })
            .cloned()
            .collect();
        messages.sort_by_key(|m| std::cmp::Reverse(chronological_key(m)));
        let more = messages.len() > PAGE_SIZE;
        messages.truncate(PAGE_SIZE);
        let next = if more {
            let (at_ms, message_id) = chronological_key(messages.last().expect("nonempty history"));
            Some(encode_cursor(&HistoryCursor {
                profile: profile.to_owned(),
                peer: peer.map(str::to_owned),
                at_ms,
                message_id,
            })?)
        } else {
            None
        };
        messages.reverse();
        Ok(
            json!({"messages":messages.into_iter().map(public_message).collect::<Vec<_>>(),"next_cursor":next,"revision":state.revision}),
        )
    }

    /// Stable conversation-key ordering prevents a busy thread moving across
    /// pages when its latest message changes. UI ordering is by latest date.
    fn conversations(
        &self,
        profile: Option<&str>,
        key: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<Value, StoreError> {
        let state = self.0.read(read_snapshot)?;
        if let Some(profile) = profile {
            require_profile(&state, profile, false)?;
        }
        if key.is_some() && cursor.is_some() {
            return Err(invalid());
        }
        let after = cursor
            .map(decode_cursor::<ConversationCursor>)
            .transpose()?;
        if after
            .as_ref()
            .is_some_and(|c| c.profile.as_deref() != profile)
        {
            return Err(invalid());
        }
        let mut threads: BTreeMap<String, (usize, Value)> = BTreeMap::new();
        for message in state
            .messages
            .values()
            .filter(|m| visible_message(m) && profile.is_none_or(|p| m["profile_id"] == p))
        {
            let profile_id = message["profile_id"].as_str().unwrap_or("");
            let peer = message["peer"].as_str().unwrap_or("");
            let thread_key = conversation_key(profile_id, peer);
            if key.is_some_and(|key| key != thread_key)
                || after.as_ref().is_some_and(|after| thread_key <= after.key)
            {
                continue;
            }
            let entry = threads
                .entry(thread_key)
                .or_insert_with(|| (0, message.clone()));
            entry.0 += 1;
            if chronological_key(message) > chronological_key(&entry.1) {
                entry.1 = message.clone();
            }
        }
        let more = threads.len() > PAGE_SIZE;
        let page: Vec<_> = threads.into_iter().take(PAGE_SIZE).collect();
        let next = if more {
            Some(encode_cursor(&ConversationCursor {
                profile: profile.map(str::to_owned),
                key: page.last().expect("nonempty summaries").0.clone(),
            })?)
        } else {
            None
        };
        let conversations: Vec<_> = page
            .into_iter()
            .map(|(key, (count, message))| {
                json!({
                    "conversation_id":key,"profile_id":message["profile_id"],"peer":message["peer"],
                    "message_count":count,"latest_message":public_message(message)
                })
            })
            .collect();
        Ok(json!({"conversations":conversations,"next_cursor":next,"revision":state.revision}))
    }

    fn read(&self, action: &str, profile: Option<&str>, cursor: u64) -> Result<Value, StoreError> {
        let state = self.0.read(read_snapshot)?;
        match action {
            "catalog" => Ok(
                json!({"profiles":state.profiles.values().collect::<Vec<_>>(),"desired":state.desired.iter().map(|(id,d)|json!({"profile_id":id,"desired_state":d.desired_state,"revision":d.revision})).collect::<Vec<_>>(),"revision":state.revision}),
            ),
            "pending" => Ok(
                json!({"messages":state.messages.values().filter(|m| m["state"]=="queued" && m["deleted"]!=true && profile.is_none_or(|p|m["profile_id"]==p)).collect::<Vec<_>>(),"revision":state.revision}),
            ),
            "messages" => {
                let profile = profile.ok_or_else(invalid)?;
                require_profile(&state, profile, false)?;
                let mut messages: Vec<_> = state
                    .messages
                    .values()
                    .filter(|m| {
                        m["profile_id"] == profile
                            && matches!(m["direction"].as_str(), Some("inbound" | "outbound"))
                            && !matches!(m["state"].as_str(), Some("partial" | "ambiguous"))
                            && m["deleted"] != true
                            && m["revision"].as_u64().unwrap_or(0) > cursor
                    })
                    .cloned()
                    .collect();
                messages.sort_by_key(|m| m["revision"].as_u64().unwrap_or(0));
                messages.truncate(PAGE_SIZE);
                let next = messages
                    .last()
                    .and_then(|m| m["revision"].as_u64())
                    .unwrap_or(state.revision);
                // Public data deliberately excludes raw protocol payloads and private attempt tokens.
                for m in &mut messages {
                    if let Some(map) = m.as_object_mut() {
                        for key in [
                            "parts",
                            "attempt_token",
                            "auto_group",
                            "group_first_at_ms",
                            "group_last_at_ms",
                            "ambiguity_reason",
                        ] {
                            map.remove(key);
                        }
                    }
                }
                Ok(json!({"messages":messages,"next_cursor":next,"revision":state.revision}))
            }
            "changes" => {
                let changes: Vec<_> = state
                    .changes
                    .iter()
                    .filter(|v| v["revision"].as_u64().unwrap_or(0) > cursor)
                    .take(PAGE_SIZE)
                    .cloned()
                    .collect();
                let next = changes
                    .last()
                    .and_then(|c| c["revision"].as_u64())
                    .unwrap_or(state.revision);
                Ok(json!({"changes":changes,"next_cursor":next,"revision":state.revision}))
            }
            _ => Err(failure("route_not_found")),
        }
    }

    /// Produces one forwarding page directly from the durable Rust archive.
    /// The page cursor is the last examined change, including changes which do
    /// not contain an eligible SMS, so deleted/partial/outbound events cannot
    /// hold the producer behind forever.
    pub(crate) fn forwarding_page(
        &self,
        cursor: u64,
    ) -> Result<(Vec<ForwardingMessage>, u64), StoreError> {
        let state = self.0.read(read_snapshot)?;
        let changes: Vec<_> = state
            .changes
            .iter()
            .filter(|change| change["revision"].as_u64().unwrap_or(0) > cursor)
            .take(PAGE_SIZE)
            .cloned()
            .collect();
        let next_cursor = changes
            .last()
            .and_then(|change| change["revision"].as_u64())
            .unwrap_or(state.revision);
        let mut seen_ids = HashSet::new();
        let mut messages = Vec::new();
        for change in changes {
            if change["kind"] != "message"
                || change["deleted"] == true
                || change["direction"] != "inbound"
                || change["state"] != "received"
            {
                continue;
            }
            let Some(profile) = change["profile_id"].as_str() else {
                continue;
            };
            let Some(message_id) = change["message_id"].as_str() else {
                continue;
            };
            let Some(message) = state.messages.get(&scope(profile, message_id)) else {
                continue;
            };
            if !visible_message(message)
                || message["direction"] != "inbound"
                || message["state"] != "received"
            {
                continue;
            }
            let created_at_ms = message["created_at_ms"].as_u64().ok_or_else(invalid)?;
            let timestamp =
                OffsetDateTime::from_unix_timestamp_nanos(i128::from(created_at_ms) * 1_000_000)
                    .map_err(|_| invalid())?
                    .format(&Rfc3339)
                    .map_err(|_| invalid())?;
            let stable_id = format!("nwait:{profile}:{message_id}");
            let forwarding_message = ForwardingMessage::new(
                stable_id.clone(),
                "inbound",
                message["peer"].as_str().unwrap_or_default(),
                message["body"].as_str().unwrap_or_default(),
                timestamp,
                "N待",
            )
            .map_err(|_| invalid())?;
            if seen_ids.insert(stable_id) {
                messages.push(forwarding_message);
            }
        }
        Ok((messages, next_cursor))
    }

    pub(crate) fn current_revision(&self) -> Result<u64, StoreError> {
        self.0.read(|items| Ok(read_snapshot(items)?.revision))
    }

    fn delete(&self, profile: &str, message_id: &str) -> Result<Value, StoreError> {
        self.0.mutate(|items| {
            let mut state = read_snapshot(items)?;
            require_profile(&state, profile, false)?;
            let key = scope(profile, message_id);
            let Some(m) = state.messages.get(&key) else {
                return Err(failure("message_not_found"));
            };
            if matches!(m["state"].as_str(), Some("queued" | "submitting")) {
                return Err(conflict());
            }
            if m["deleted"] == true {
                return Ok(json!({"deleted":true,"revision":state.revision}));
            }
            let revision = next_revision(&mut state)?;
            let m = state.messages.get_mut(&key).expect("validated message");
            if let Some(peer) = m["peer"].as_str() {
                m["conversation_id"] = json!(conversation_key(profile, peer));
            }
            m["deleted"] = json!(true);
            m["revision"] = json!(revision);
            if let Some(record) = m.as_object_mut() {
                for key in [
                    "body",
                    "peer",
                    "phone",
                    "parts",
                    "raw_rpdu_hex",
                    "raw_tpdu_hex",
                ] {
                    record.remove(key);
                }
            }
            change(&mut state, profile, message_id);
            persist_snapshot(items, state)?;
            Ok(json!({"deleted":true,"revision":revision}))
        })
    }
}

pub(crate) fn random_token() -> Result<String, StoreError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| failure("entropy_unavailable"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub(crate) fn internal_authenticated<B>(request: &Request<B>, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let Some(value) = request
        .headers()
        .get(INTERNAL_HEADER)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    value.len() == expected.len() && bool::from(value.as_bytes().ct_eq(expected.as_bytes()))
}
pub(crate) fn is_public_route(method: &Method, path: &str) -> bool {
    (method == Method::GET
        && matches!(
            path,
            "/api/multistandby/messages"
                | "/api/multistandby/changes"
                | "/api/multistandby/history"
                | "/api/multistandby/conversations"
                | "/api/multistandby/cards"
                | "/api/multistandby/profiles"
                | "/api/multistandby/send-intents"
        ))
        || (method == Method::POST
            && matches!(
                path,
                "/api/multistandby/messages/send"
                    | "/api/multistandby/send-intents"
                    | "/api/multistandby/send-intents/submit"
            ))
        || (method == Method::DELETE && path.starts_with("/api/multistandby/messages/"))
}
/// Joins the native runtime with the original durable catalog and desired state.
/// Offline fallbacks preserve the established public API shape.
pub(crate) async fn handle_profiles(
    request: Request<Incoming>,
    store: Store,
    runtime_service: std::sync::Arc<crate::multistandby_native_service::Service>,
) -> Response<Full<Bytes>> {
    let health = store.clone();
    if let Some(response) = crate::sms_store_health::rejection(store.status()) {
        return response;
    }
    let card_filter = profile_card_filter(request.uri().query().unwrap_or_default());
    let runtime = match runtime_service.snapshot(&card_filter, false, false).await {
        Ok(value) => value,
        Err(error) => {
            return finish_response(
                crate::json_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &json!({"error":error.to_string(),"code":"runtime_snapshot_unavailable","retryable":true}),
                ),
                &health,
            );
        }
    };

    let catalog_store = store.clone();
    let catalog = match crate::archive_io::run(move || catalog_store.read("catalog", None, 0)).await
    {
        Ok(catalog) => catalog,
        Err(_) => {
            return finish_response(
                crate::json_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &json!({"error":"多待存储尚未就绪"}),
                ),
                &health,
            );
        }
    };
    let result = project_profiles(&runtime, &catalog, &card_filter);
    let response = match result {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(error) => error_response(error),
    };
    finish_response(response, &health)
}

/// Projects the retained native physical inventory using the original API shape.
pub(crate) async fn handle_cards(
    store: Store,
    runtime_service: std::sync::Arc<crate::multistandby_native_service::Service>,
) -> Response<Full<Bytes>> {
    let health = store.clone();
    let runtime = match runtime_service.snapshot("", true, false).await {
        Ok(value) => value,
        Err(error) => {
            return finish_response(
                crate::json_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &json!({"error":error.to_string(),"code":"runtime_snapshot_unavailable","retryable":true}),
                ),
                &health,
            );
        }
    };

    let response = match project_cards(&runtime) {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(_) => crate::json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"error":"多待卡片清单暂不可读取","code":"runtime_snapshot_invalid","retryable":true}),
        ),
    };
    finish_response(response, &health)
}

fn project_cards(runtime: &Value) -> Result<Value, StoreError> {
    let sources = runtime["card_profiles"].as_array().ok_or_else(invalid)?;
    let available = runtime["available"].as_bool().ok_or_else(invalid)?;
    let detail = runtime["detail"].as_str().ok_or_else(invalid)?;
    let boot_id = runtime["boot_id"].as_str().ok_or_else(invalid)?;
    let mut indices: HashMap<String, usize> = HashMap::new();
    let mut cards: Vec<Value> = Vec::new();
    for source in sources {
        let card_id = source["card_id"].as_str().ok_or_else(invalid)?;
        let reader = source["reader"].as_str().ok_or_else(invalid)?;
        let eid = source["eid"].as_str().ok_or_else(invalid)?;
        if let Some(index) = indices.get(card_id).copied() {
            let count = cards[index]["profile_count"].as_u64().ok_or_else(invalid)?;
            cards[index]["profile_count"] = json!(count + 1);
            continue;
        }
        indices.insert(card_id.to_owned(), cards.len());
        cards.push(json!({
            "card_id":card_id,
            "reader":reader,
            "eid":eid,
            "profile_count":1,
            "available":available,
            "detail":detail
        }));
    }
    Ok(json!({"cards":cards,"boot_id":boot_id,"detail":detail}))
}

fn project_profiles(
    runtime: &Value,
    catalog: &Value,
    card_filter: &str,
) -> Result<Value, StoreError> {
    let live_profiles = runtime["profiles"].as_array().ok_or_else(invalid)?;
    let catalog_profiles = catalog["profiles"].as_array().ok_or_else(invalid)?;
    let desired = catalog["desired"].as_array().ok_or_else(invalid)?;
    let boot_id = runtime["boot_id"].as_str().ok_or_else(invalid)?;
    let revision = catalog["revision"].as_u64().ok_or_else(invalid)?;
    let mut desired_by_profile = BTreeMap::new();
    for item in desired {
        let profile_id = item["profile_id"].as_str().ok_or_else(invalid)?;
        let desired_state = item["desired_state"].as_str().ok_or_else(invalid)?;
        desired_by_profile.insert(profile_id, desired_state);
    }

    let mut seen = HashSet::new();
    let mut profiles = Vec::with_capacity(live_profiles.len() + catalog_profiles.len());
    for profile in live_profiles {
        let profile_id = profile["profile_id"].as_str().ok_or_else(invalid)?;
        if card_filter.is_empty() || profile["card_id"].as_str() == Some(card_filter) {
            seen.insert(profile_id.to_owned());
            profiles.push(profile.clone());
        }
    }
    for profile in catalog_profiles {
        let profile_id = profile["profile_id"].as_str().ok_or_else(invalid)?;
        if seen.contains(profile_id)
            || (!card_filter.is_empty() && profile["card_id"].as_str() != Some(card_filter))
        {
            continue;
        }
        let mut offline = profile.clone();
        let object = offline.as_object_mut().ok_or_else(invalid)?;
        object.remove("tombstone");
        object.insert("physical_enabled".to_owned(), json!(false));
        object.insert(
            "desired_state".to_owned(),
            json!(
                desired_by_profile
                    .get(profile_id)
                    .copied()
                    .unwrap_or("stopped")
            ),
        );
        object.insert("auth_state".to_owned(), json!(""));
        object.insert("tunnel_state".to_owned(), json!(""));
        object.insert("ims_state".to_owned(), json!(""));
        object.insert("sms_ready".to_owned(), json!(false));
        object.insert("health".to_owned(), json!("offline"));
        object.insert(
            "last_error".to_owned(),
            json!("卡片当前不可用；历史短信仍可查看"),
        );
        object.insert("session_generation".to_owned(), json!(0));
        object.insert("consecutive_failures".to_owned(), json!(0));
        profiles.push(offline);
    }
    Ok(json!({"profiles":profiles,"boot_id":boot_id,"revision":revision}))
}

fn profile_card_filter(query: &str) -> String {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "card_id")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}
pub(crate) fn shutdown_write_allowed(path: &str) -> bool {
    matches!(
        path,
        "/api/internal/multistandby/store/inbound"
            | "/api/internal/multistandby/store/inbound-event"
            | "/api/internal/multistandby/store/outcome"
    )
}
fn error_response(error: StoreError) -> Response<Full<Bytes>> {
    let code = match &error {
        StoreError::Configuration(code) => code.as_str(),
        _ => "store_io_failed",
    };
    let status = match code {
        "invalid_request" => StatusCode::BAD_REQUEST,
        "profile_not_found" | "message_not_found" | "send_intent_not_found" | "route_not_found" => {
            StatusCode::NOT_FOUND
        }
        "state_conflict" | "profile_stopped" | "profile_deleted" => StatusCode::CONFLICT,
        "sms_store_unavailable" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    crate::json_response(
        status,
        &json!({"error":"多待存储操作未完成","code":code,"retryable":false}),
    )
}
fn query(request: &Request<Incoming>) -> Result<BTreeMap<String, String>, StoreError> {
    let mut out = BTreeMap::new();
    for pair in request
        .uri()
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|s| !s.is_empty())
    {
        let (key, value) = pair.split_once('=').ok_or_else(invalid)?;
        // IDs and cursors are opaque URL-safe tokens in this contract.
        if !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
            || out.insert(key.to_owned(), value.to_owned()).is_some()
        {
            return Err(invalid());
        }
    }
    Ok(out)
}
pub(crate) async fn handle(
    request: Request<Incoming>,
    store: Store,
    internal: bool,
) -> Response<Full<Bytes>> {
    let health = store.clone();
    let response = handle_inner(request, store, internal).await;
    finish_response(response, &health)
}

fn finish_response(response: Response<Full<Bytes>>, store: &Store) -> Response<Full<Bytes>> {
    if response.status().is_server_error() {
        if let Some(unavailable) = crate::sms_store_health::rejection(store.status()) {
            return unavailable;
        }
    }
    response
}

async fn handle_inner(
    request: Request<Incoming>,
    store: Store,
    internal: bool,
) -> Response<Full<Bytes>> {
    if let Some(response) = crate::sms_store_health::rejection(store.status()) {
        return response;
    }
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let params = match query(&request) {
        Ok(v) => v,
        Err(e) => return error_response(e),
    };
    if method == Method::GET && !internal && path == "/api/multistandby/send-intents" {
        if params
            .keys()
            .any(|key| !matches!(key.as_str(), "profile_id" | "client_request_id" | "cursor"))
        {
            return error_response(invalid());
        }
        let result = crate::archive_io::run(move || {
            store.send_intents(
                params.get("profile_id").map(String::as_str),
                params.get("client_request_id").map(String::as_str),
                params.get("cursor").map(String::as_str),
            )
        })
        .await;
        return match result {
            Ok(v) => crate::json_response(StatusCode::OK, &v),
            Err(e) => error_response(e),
        };
    }
    if method == Method::GET
        && !internal
        && matches!(
            path.as_str(),
            "/api/multistandby/history" | "/api/multistandby/conversations"
        )
    {
        if params.keys().any(|key| {
            !matches!(
                key.as_str(),
                "profile_id" | "cursor" | "peer_hex" | "conversation_id"
            )
        }) {
            return error_response(invalid());
        }
        let result = crate::archive_io::run(move || {
            let profile = params.get("profile_id").map(String::as_str);
            let cursor = params.get("cursor").map(String::as_str);
            if path.ends_with("/history") {
                if params.contains_key("conversation_id") {
                    return Err(invalid());
                }
                let peer = params
                    .get("peer_hex")
                    .map(|value| decode_peer(value))
                    .transpose()?;
                store.history(profile.ok_or_else(invalid)?, peer.as_deref(), cursor)
            } else {
                if params.contains_key("peer_hex") {
                    return Err(invalid());
                }
                store.conversations(
                    profile,
                    params.get("conversation_id").map(String::as_str),
                    cursor,
                )
            }
        })
        .await;
        return match result {
            Ok(v) => crate::json_response(StatusCode::OK, &v),
            Err(e) => error_response(e),
        };
    }
    let cursor = match params.get("cursor").map(|s| s.parse::<u64>()).transpose() {
        Ok(v) => v.unwrap_or(0),
        Err(_) => return error_response(invalid()),
    };
    let profile = params.get("profile_id").cloned();
    if method == Method::GET {
        let action = if internal {
            path.strip_prefix(INTERNAL_PREFIX).unwrap_or("")
        } else if path.ends_with("/changes") {
            "changes"
        } else {
            "messages"
        }
        .to_owned();
        if (internal && !matches!(action.as_str(), "catalog" | "pending"))
            || (!internal && !is_public_route(&method, &path))
        {
            return error_response(failure("route_not_found"));
        }
        return match crate::archive_io::run(move || store.read(&action, profile.as_deref(), cursor))
            .await
        {
            Ok(v) => crate::json_response(StatusCode::OK, &v),
            Err(e) => error_response(e),
        };
    }
    if !internal && method == Method::DELETE {
        let Some(profile) = profile else {
            return error_response(invalid());
        };
        let message_id = path
            .trim_start_matches("/api/multistandby/messages/")
            .to_owned();
        return match crate::archive_io::run(move || store.delete(&profile, &message_id)).await {
            Ok(v) => crate::json_response(StatusCode::OK, &v),
            Err(e) => error_response(e),
        };
    }
    let public_intent = !internal
        && method == Method::POST
        && matches!(
            path.as_str(),
            "/api/multistandby/send-intents" | "/api/multistandby/send-intents/submit"
        );
    if (!internal && !public_intent) || method != Method::POST {
        return error_response(failure("route_not_found"));
    }
    if public_intent && !params.is_empty() {
        return error_response(invalid());
    }
    let bytes = match crate::http_server::collect_body(request.into_body(), MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Err(crate::http_server::BodyReadError::TooLarge) => {
            return crate::json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                &json!({"code":"body_too_large"}),
            );
        }
        Err(crate::http_server::BodyReadError::Invalid) => return error_response(invalid()),
    };
    let body = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => v,
        Err(_) => return error_response(invalid()),
    };
    if public_intent {
        let submit = path.ends_with("/submit");
        return match crate::archive_io::run(move || store.prepare_send_intent(body, submit)).await {
            Ok(v) => crate::json_response(StatusCode::OK, &v),
            Err(e) => error_response(e),
        };
    }
    let action = path.strip_prefix(INTERNAL_PREFIX).unwrap_or("").to_owned();
    // The production HTTP writer may submit only raw discovery facts. The
    // legacy ID-shaped catalog form remains available to the direct store
    // fixture/migration reader, never to a Go caller over IPC.
    if internal && action == "catalog" && body.get("reader").is_none() {
        return error_response(invalid());
    }
    if internal && action == "recovery-selection" {
        let Some(object) = body.as_object() else {
            return error_response(invalid());
        };
        if object.len() != 1 || !object.contains_key("profile_ids") {
            return error_response(invalid());
        }
        let Some(values) = body["profile_ids"].as_array() else {
            return error_response(invalid());
        };
        if values.len() > 4096 {
            return error_response(invalid());
        }
        let mut ids = Vec::with_capacity(values.len());
        let mut unique = HashSet::with_capacity(values.len());
        for value in values {
            let parsed = match id(&json!({"profile_id":value}), "profile_id") {
                Ok(value) => value,
                Err(error) => return error_response(error),
            };
            if !unique.insert(parsed.clone()) {
                return error_response(invalid());
            }
            ids.push(parsed);
        }
        return match crate::archive_io::run(move || store.recovery_selection(&ids)).await {
            Ok(value) => crate::json_response(StatusCode::OK, &value),
            Err(error) => error_response(error),
        };
    }
    match crate::archive_io::run(move || store.apply(&action, body)).await {
        Ok(v) => crate::json_response(StatusCode::OK, &v),
        Err(e) => error_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn first_worker_panic_response_reports_latched_store_unavailable() {
        let f = Fixture::new();
        let worker = f.store.clone();
        let executor = crate::archive_io::IsolatedTestExecutor::new();
        let result = executor
            .run(move || {
                worker
                    .0
                    .mutate::<()>(|_| panic!("synthetic worker failure"))
            })
            .await;
        let response = finish_response(error_response(result.unwrap_err()), &f.store);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(f.store.status().reason, Some("store_worker_failed"));
        assert!(
            executor
                .finish_pending()
                .await
                .unwrap_err()
                .contains("unconfirmed")
        );
    }

    struct Fixture {
        root: std::path::PathBuf,
        store: Store,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("sw-store-{}", random_token().unwrap()));
            let store = Store::open(&root).unwrap();
            store.recover().unwrap();
            store.apply("catalog",json!({"event_id":"catalog-1","profiles":[{"profile_id":"A","card_id":"C","eid":"E","iccid":"1"},{"profile_id":"B","card_id":"C","eid":"E","iccid":"2"}]})).unwrap();
            for p in ["A", "B"] {
                store.apply("desired",json!({"event_id":format!("start-{p}"),"profile_id":p,"desired_state":"keep_online"})).unwrap();
            }
            Self { root, store }
        }
        fn queue(&self, p: &str, id: &str) -> Value {
            self.store.apply("queue",json!({"event_id":format!("queue-{p}-{id}"),"profile_id":p,"client_request_id":id,"phone":"+1000","body":"hello"})).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
    #[test]
    fn rust_catalog_discovery_owns_stable_physical_resource_ids() {
        let f = Fixture::new();
        let response = f
            .store
            .apply(
                "catalog",
                json!({
                    "event_id":"discover-1",
                    "reader":"BLE:reader-A",
                    "profiles":[{"eid":"eid-A","iccid":"iccid-A","display_name":"Carrier A"}]
                }),
            )
            .unwrap();
        assert_eq!(
            response["profiles"][0]["profile_id"],
            "profile_737e782af757ceebad3f527594b5252a"
        );
        assert_eq!(
            response["profiles"][0]["card_id"],
            "card_43da34335ae84c0f0a3fe2d4a593cee3"
        );
        assert_eq!(response["profiles"][0]["eid"], "eid-A");
        assert_eq!(response["profiles"][0]["iccid"], "iccid-A");
        let catalog = f.store.read("catalog", None, 0).unwrap();
        assert!(
            catalog["profiles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|profile| {
                    profile["profile_id"] == response["profiles"][0]["profile_id"]
                        && profile["display_name"] == "Carrier A"
                })
        );
        assert_eq!(
            f.store
                .apply(
                    "catalog",
                    json!({
                        "event_id":"discover-1",
                        "reader":"BLE:reader-A",
                        "profiles":[{"eid":"eid-A","iccid":"iccid-A","display_name":"Carrier A"}]
                    }),
                )
                .unwrap(),
            response,
            "event replay must return the original identity mapping"
        );
    }

    #[test]
    fn rust_catalog_discovery_rejects_ambiguous_or_unbounded_identity_facts() {
        let f = Fixture::new();
        for body in [
            json!({"event_id":"missing-reader","profiles":[{"eid":"e","iccid":"i"}]}),
            json!({"event_id":"blank-id","reader":"reader","profiles":[{"eid":"e","iccid":" "}]}),
            json!({"event_id":"duplicate","reader":"reader","profiles":[{"eid":"e","iccid":"i"},{"eid":"e","iccid":"i"}]}),
            json!({"event_id":"normalized-duplicate","reader":"reader","profiles":[{"eid":"e","iccid":"123F"},{"eid":"e","iccid":"123"}]}),
            json!({"event_id":"unknown-field","reader":"reader","profiles":[{"eid":"e","iccid":"i","imsi":"secret"}]}),
        ] {
            assert!(matches!(
                f.store.apply("catalog", body).unwrap_err(),
                StoreError::Configuration(code) if code == "invalid_request"
            ));
        }
    }
    fn incoming(p: &str, event: &str, key: &str, message: &str, body: &str) -> Value {
        json!({"event_id":event,"dedup_key":key,"profile_id":p,"message_id":message,"sender":"+1000","body":body,"received_at_ms":123})
    }

    fn inbound_event(
        profile: &str,
        event_id: &str,
        rpdu: &str,
        content: &str,
        received_at: u64,
        sequence: i64,
    ) -> Value {
        json!({
            "event_id":event_id,
            "profile_id":profile,
            "sender":"+1000",
            "content":content,
            "timestamp_ms":received_at,
            "timestamp_missing":false,
            "raw_rpdu_hex":rpdu,
            "raw_tpdu_hex":"aabb",
            "decoded":true,
            "status_report":false,
            "delivery_status":"",
            "status_code":0,
            "concat":{"reference":42,"reference_bits":8,"sequence":sequence,"total":2}
        })
    }

    fn send_intent_body(request: &str) -> Value {
        json!({"profile_id":"A","client_request_id":request,"phone":"+1000","body":"hello"})
    }

    #[test]
    fn send_intent_survives_restart_and_only_first_confirmed_authorization_can_submit() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let revision = f.store.read("changes", None, 0).unwrap()["revision"].clone();
        assert_eq!(
            f.store
                .prepare_send_intent(send_intent_body("intent"), false)
                .unwrap()["state"],
            "prepared"
        );
        let restarted = Store::open(&f.root).unwrap();
        restarted.recover().unwrap();
        let prepared = restarted
            .send_intents(Some("A"), Some("intent"), None)
            .unwrap();
        assert_eq!(prepared["state"], "prepared");
        assert_eq!(prepared["can_submit"], false);
        assert!(
            restarted.read("pending", None, 0).unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            restarted
                .prepare_send_intent(send_intent_body("intent"), true)
                .unwrap()["can_submit"],
            true
        );
        assert_eq!(
            restarted
                .prepare_send_intent(send_intent_body("intent"), true)
                .unwrap()["can_submit"],
            false
        );
        let reloaded = Store::open(&f.root).unwrap();
        assert_eq!(
            reloaded
                .send_intents(Some("A"), Some("intent"), None)
                .unwrap()["state"],
            "unknown"
        );
        let mut changed = send_intent_body("intent");
        changed["body"] = json!("changed");
        assert!(
            reloaded
                .prepare_send_intent(changed.clone(), false)
                .is_err()
        );
        changed["event_id"] = json!("legacy-mutated-payload");
        assert!(reloaded.apply("queue", changed).is_err());
        assert!(
            reloaded
                .send_intents(Some("B"), Some("intent"), None)
                .is_err()
        );
        assert_eq!(
            reloaded.read("changes", None, 0).unwrap()["revision"],
            revision
        );
        let queued = f.queue("A", "intent");
        assert!(
            read_snapshot(&f.store.0.load().unwrap())
                .unwrap()
                .send_intents
                .is_empty()
        );
        assert_eq!(
            f.store
                .send_intents(Some("A"), Some("intent"), None)
                .unwrap()["state"],
            "queued"
        );
        f.store.apply("claim",json!({"event_id":"intent-claim","profile_id":"A","message_id":queued["message_id"],"attempt_token":"attempt"})).unwrap();
        f.store.recover().unwrap();
        let result = f
            .store
            .send_intents(Some("A"), Some("intent"), None)
            .unwrap();
        assert_eq!(result["state"], "unknown");
        assert_eq!(result["message_id"], queued["message_id"]);
        assert_eq!(
            f.store.send_intents(None, None, None).unwrap()["intents"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let fresh = f.queue("A", "explicit-fresh-intent");
        assert_ne!(fresh["message_id"], queued["message_id"]);
        assert_eq!(
            std::fs::metadata(f.store.0.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(f.store.0.path().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn forwarding_page_uses_stable_profile_scoped_ids_and_skips_deleted_messages() {
        let f = Fixture::new();
        f.store
            .apply(
                "inbound",
                incoming("A", "inbound-1", "dedup-1", "message-1", "hello"),
            )
            .unwrap();
        let (page, cursor) = f.store.forwarding_page(0).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id(), "nwait:A:message-1");
        assert_eq!(page[0].direction(), "inbound");
        assert_eq!(page[0].sender(), "+1000");
        assert_eq!(page[0].content(), "hello");
        assert_eq!(page[0].device(), "N待");
        assert!(cursor > 0);

        f.store.delete("A", "message-1").unwrap();
        let (after_delete, _) = f.store.forwarding_page(0).unwrap();
        assert!(after_delete.is_empty());
    }

    #[test]
    fn send_intent_failed_persistence_cannot_grant_submission_permission() {
        let f = Fixture::new();
        let before = std::fs::read(f.store.0.path()).unwrap();
        let limited = Store(
            ArchiveStore::with_limits(f.store.0.path(), before.len() as u64, 1)
                .unwrap()
                .into(),
        );
        assert!(
            limited
                .prepare_send_intent(send_intent_body("limited"), false)
                .is_err()
        );
        assert_eq!(std::fs::read(f.store.0.path()).unwrap(), before);
        assert!(
            f.store
                .send_intents(Some("A"), Some("limited"), None)
                .is_err()
        );
        f.store
            .prepare_send_intent(send_intent_body("limited"), false)
            .unwrap();
        let prepared = std::fs::read(f.store.0.path()).unwrap();
        let limited = Store(
            ArchiveStore::with_limits(f.store.0.path(), prepared.len() as u64, 1)
                .unwrap()
                .into(),
        );
        assert!(
            limited
                .prepare_send_intent(send_intent_body("limited"), true)
                .is_err()
        );
        assert_eq!(std::fs::read(f.store.0.path()).unwrap(), prepared);
        assert_eq!(
            f.store
                .send_intents(Some("A"), Some("limited"), None)
                .unwrap()["state"],
            "prepared"
        );
    }

    #[test]
    fn send_intent_recovery_pages_are_bounded_and_survive_consumed_anchor() {
        let f = Fixture::new();
        for index in 0..151 {
            f.store
                .prepare_send_intent(send_intent_body(&format!("intent-{index:03}")), false)
                .unwrap();
        }
        let first = f.store.send_intents(None, None, None).unwrap();
        assert_eq!(first["intents"].as_array().unwrap().len(), 100);
        let cursor = first["next_cursor"].as_str().unwrap();
        let consumed = first["intents"][99]["client_request_id"].as_str().unwrap();
        f.queue("A", consumed);
        let second = f.store.send_intents(None, None, Some(cursor)).unwrap();
        assert_eq!(second["intents"].as_array().unwrap().len(), 51);
        assert_eq!(second["next_cursor"], Value::Null);
        assert!(first["intents"].as_array().unwrap().iter().all(|intent| {
            !second["intents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|other| other["client_request_id"] == intent["client_request_id"])
        }));
        let queued = f
            .store
            .send_intents(Some("A"), Some(consumed), None)
            .unwrap();
        f.store.apply("claim",json!({"event_id":"delete-intent-claim","profile_id":"A","message_id":queued["message_id"],"attempt_token":"attempt"})).unwrap();
        f.store.apply("outcome",json!({"event_id":"delete-intent-outcome","profile_id":"A","message_id":queued["message_id"],"attempt_token":"attempt","state":"accepted"})).unwrap();
        f.store
            .delete("A", queued["message_id"].as_str().unwrap())
            .unwrap();
        let deleted = f
            .store
            .send_intents(Some("A"), Some(consumed), None)
            .unwrap();
        assert_eq!(deleted["state"], "cancelled");
        assert_eq!(deleted["body"], "");
    }

    #[test]
    fn history_latest_equal_time_pages_survive_deleted_anchor_and_keep_feed_unchanged() {
        let f = Fixture::new();
        for index in 0..250 {
            let key = format!("message-{index:03}");
            let mut body = incoming("A", &key, &key, &key, "text");
            body["sender"] = json!("+2000");
            f.store.apply("inbound", body).unwrap();
        }
        let mut other = incoming(
            "B",
            "other-profile",
            "other-profile",
            "message-150",
            "other card",
        );
        other["sender"] = json!("+2000");
        f.store.apply("inbound", other).unwrap();
        let before = f.store.read("changes", None, 0).unwrap();
        let reopened = Store::open(&f.root).unwrap();
        let first = reopened.history("A", Some("+2000"), None).unwrap();
        assert_eq!(first["messages"].as_array().unwrap().len(), 100);
        assert_eq!(first["messages"][0]["message_id"], "message-150");
        assert_eq!(first["messages"][99]["message_id"], "message-249");
        assert_eq!(reopened.read("changes", None, 0).unwrap(), before);
        let cursor = first["next_cursor"].as_str().unwrap();
        assert!(reopened.history("B", Some("+2000"), Some(cursor)).is_err());
        assert!(reopened.history("A", Some("+other"), Some(cursor)).is_err());
        assert!(
            reopened
                .history("A", Some("+2000"), Some("invalid"))
                .is_err()
        );
        reopened.delete("A", "message-150").unwrap();
        let mut arrived = incoming("A", "late-arrival", "late-arrival", "late-arrival", "newer");
        arrived["sender"] = json!("+2000");
        arrived["received_at_ms"] = json!(124);
        reopened.apply("inbound", arrived).unwrap();
        let second = reopened.history("A", Some("+2000"), Some(cursor)).unwrap();
        assert_eq!(second["messages"].as_array().unwrap().len(), 100);
        assert_eq!(second["messages"][0]["message_id"], "message-050");
        assert_eq!(second["messages"][99]["message_id"], "message-149");
        let third = reopened
            .history("A", Some("+2000"), second["next_cursor"].as_str())
            .unwrap();
        assert_eq!(third["messages"].as_array().unwrap().len(), 50);
        assert!(third["next_cursor"].is_null());
        assert_eq!(
            reopened.history("B", Some("+2000"), None).unwrap()["messages"][0]["message_id"],
            "message-150"
        );
        let changes = reopened.read("changes", None, 250).unwrap();
        let deletion = changes["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|change| change["deleted"] == true)
            .unwrap();
        assert_eq!(deletion["conversation_id"], conversation_key("A", "+2000"));
        assert!(deletion.get("peer").is_none());
    }

    #[test]
    fn conversation_summaries_cover_quiet_threads_and_remain_stable_when_busy_thread_updates() {
        let f = Fixture::new();
        for index in 0..131 {
            let key = format!("quiet-{index:03}");
            let mut body = incoming("A", &key, &key, &key, "old");
            body["sender"] = json!(format!("peer-{index:03}"));
            f.store.apply("inbound", body).unwrap();
        }
        for index in 0..150 {
            let key = format!("active-{index:03}");
            let mut body = incoming("A", &key, &key, &key, "new");
            body["sender"] = json!("busy");
            body["received_at_ms"] = json!(500);
            f.store.apply("inbound", body).unwrap();
        }
        let first = f.store.conversations(Some("A"), None, None).unwrap();
        assert_eq!(first["conversations"].as_array().unwrap().len(), 100);
        let cursor = first["next_cursor"].as_str().unwrap();
        let mut update = incoming("A", "busy-update", "busy-update", "busy-update", "latest");
        update["sender"] = json!("busy");
        update["received_at_ms"] = json!(600);
        f.store.apply("inbound", update).unwrap();
        let second = f
            .store
            .conversations(Some("A"), None, Some(cursor))
            .unwrap();
        assert_eq!(second["conversations"].as_array().unwrap().len(), 32);
        let keys = first["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["conversations"].as_array().unwrap())
            .map(|conversation| conversation["conversation_id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(keys.len(), 132);
        assert!(
            f.store
                .conversations(Some("B"), None, Some(cursor))
                .is_err()
        );
        let key = conversation_key("A", "busy");
        let busy = f.store.conversations(None, Some(&key), None).unwrap();
        assert_eq!(busy["conversations"][0]["message_count"], 151);
        assert_eq!(
            busy["conversations"][0]["latest_message"]["message_id"],
            "busy-update"
        );
        f.store.delete("A", "busy-update").unwrap();
        let busy = f.store.conversations(None, Some(&key), None).unwrap();
        assert_eq!(busy["conversations"][0]["message_count"], 150);
        assert_eq!(
            busy["conversations"][0]["latest_message"]["message_id"],
            "active-149"
        );
        assert!(
            f.store
                .conversations(None, Some(&key), Some(cursor))
                .is_err()
        );
    }

    #[test]
    fn profiles_and_network_dedup_remain_isolated_across_reload() {
        let f = Fixture::new();
        for p in ["A", "B"] {
            f.store
                .apply(
                    "inbound",
                    incoming(
                        p,
                        &format!("event-{p}"),
                        "same-wire-id",
                        "same-message",
                        "hello",
                    ),
                )
                .unwrap();
        }
        let mut retry = incoming("A", "retry", "same-wire-id", "same-message", "hello");
        retry["received_at_ms"] = json!(456);
        assert!(
            f.store.apply("inbound", retry).unwrap()["complete"]
                .as_bool()
                .unwrap()
        );
        let reloaded = Store::open(&f.root).unwrap();
        assert_eq!(
            reloaded.read("messages", Some("A"), 0).unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        reloaded.delete("A", "same-message").unwrap();
        assert!(
            reloaded.read("messages", Some("A"), 0).unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reloaded.read("messages", Some("B"), 0).unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        reloaded.apply("catalog",json!({"event_id":"removed-A","profiles":[{"profile_id":"A","card_id":"C","tombstone":true}]})).unwrap();
        assert!(reloaded.read("messages", Some("A"), 0).is_ok());
    }

    #[test]
    fn rust_inbound_event_owner_preserves_network_identity_retry_and_profile_scope() {
        let f = Fixture::new();
        let first = inbound_event("A", "event-a1", "010203", "hello ", 10_000, 1);
        let first_result = f.store.apply("inbound-event", first).unwrap();
        assert_eq!(first_result["state"], "partial");
        let snapshot = read_snapshot(&f.store.0.load().unwrap()).unwrap();
        assert!(
            snapshot
                .inbound_dedup
                .contains_key("A\0mt_5728d0b0ada2710f07a81e2c355fe743")
        );

        let retry = inbound_event("A", "event-a2", "010203", "hello ", 11_000, 1);
        assert_eq!(
            f.store.apply("inbound-event", retry).unwrap(),
            first_result,
            "a network retry must resolve to the existing persisted fragment"
        );

        let second = inbound_event("A", "event-a3", "010204", "world", 12_000, 2);
        let completed = f.store.apply("inbound-event", second).unwrap();
        assert_eq!(completed["state"], "received");
        assert_eq!(completed["message_id"], first_result["message_id"]);

        let other_profile = inbound_event("B", "event-b1", "010203", "hello ", 10_000, 1);
        let isolated = f.store.apply("inbound-event", other_profile).unwrap();
        assert_eq!(isolated["state"], "partial");
        assert_ne!(isolated["message_id"], first_result["message_id"]);

        let messages = f.store.read("messages", Some("A"), 0).unwrap();
        assert_eq!(messages["messages"].as_array().unwrap().len(), 1);
        assert_eq!(messages["messages"][0]["body"], "hello world");
    }

    #[test]
    fn rust_inbound_event_owner_quarantines_conflicting_concat_and_keeps_status_reports() {
        let f = Fixture::new();
        let first = inbound_event("A", "collision-1", "1100", "original ", 10_000, 1);
        assert_eq!(
            f.store.apply("inbound-event", first).unwrap()["state"],
            "partial"
        );
        let conflicting = inbound_event("A", "collision-2", "2200", "different ", 11_000, 1);
        assert_eq!(
            f.store.apply("inbound-event", conflicting).unwrap()["state"],
            "undecoded"
        );
        assert!(
            f.store.read("messages", Some("A"), 0).unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let report = json!({
            "event_id":"report-1",
            "profile_id":"A",
            "sender":"",
            "content":"",
            "timestamp_ms":12_000,
            "timestamp_missing":false,
            "raw_rpdu_hex":"33",
            "raw_tpdu_hex":"44",
            "decoded":true,
            "status_report":true,
            "delivery_status":"delivered",
            "status_code":0,
            "message_reference":7
        });
        assert_eq!(
            f.store.apply("inbound-event", report).unwrap()["state"],
            "unmatched"
        );
    }

    #[test]
    fn multipart_journal_survives_restart_before_completion_and_reports_do_not_notify() {
        let f = Fixture::new();
        let mut first = incoming("A", "part-1", "wire-1", "multi", "hello ");
        first["part"] = json!({"reference":"r1","index":1,"total":2});
        assert_eq!(f.store.apply("inbound", first).unwrap()["state"], "partial");
        assert!(
            f.store.read("messages", Some("A"), 0).unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            f.store.read("changes", None, 0).unwrap()["changes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let reloaded = Store::open(&f.root).unwrap();
        reloaded.recover().unwrap();
        let mut second = incoming("A", "part-2", "wire-2", "multi", "world");
        second["part"] = json!({"reference":"r1","index":2,"total":2});
        assert_eq!(
            reloaded.apply("inbound", second).unwrap()["state"],
            "received"
        );
        assert_eq!(
            reloaded.read("messages", Some("A"), 0).unwrap()["messages"][0]["body"],
            "hello world"
        );
        reloaded.apply("inbound",json!({"event_id":"report","dedup_key":"report-1","profile_id":"A","message_id":"rp1","kind":"status_report","raw_tpdu_hex":"00ff","report":{"tp_mr":1}})).unwrap();
        assert_eq!(
            reloaded.read("changes", None, 0).unwrap()["changes"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            read_snapshot(&reloaded.0.load().unwrap())
                .unwrap()
                .messages
                .values()
                .filter(|m| m["state"] == "unmatched")
                .count(),
            1
        );
    }

    fn auto_part(event: &str, index: u64, total: u64, received: u64, body: &str) -> Value {
        let mut value = incoming(
            "A",
            event,
            &format!("wire-{event}"),
            &format!("tx-{event}"),
            body,
        );
        value["received_at_ms"] = json!(received);
        value["raw_tpdu_hex"] = json!(stable_digest_hex(event.as_bytes()));
        value["part"] = json!({"reference":"8:42","index":index,"total":total,"auto_group":true});
        value
    }

    #[test]
    fn auto_multipart_cross_second_fragments_survive_restart_and_deduplicate() {
        let f = Fixture::new();
        let first = auto_part("auto-first", 1, 2, 10_000, "hello ");
        let first_result = f.store.apply("inbound", first.clone()).unwrap();
        assert_eq!(first_result["state"], "partial");
        let mut duplicate = first;
        duplicate["event_id"] = json!("auto-first-retry");
        duplicate["received_at_ms"] = json!(11_000);
        assert_eq!(f.store.apply("inbound", duplicate).unwrap(), first_result);
        let reloaded = Store::open(&f.root).unwrap();
        reloaded.recover().unwrap();
        let result = reloaded
            .apply("inbound", auto_part("auto-second", 2, 2, 12_000, "world"))
            .unwrap();
        assert_eq!(result["state"], "received");
        assert_eq!(result["message_id"], first_result["message_id"]);
        let public = reloaded.read("messages", Some("A"), 0).unwrap();
        assert_eq!(public["messages"].as_array().unwrap().len(), 1);
        assert_eq!(public["messages"][0]["body"], "hello world");
        assert!(public["messages"][0].get("auto_group").is_none());
        assert_eq!(
            reloaded.read("changes", None, 0).unwrap()["changes"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn auto_multipart_completed_reference_reuse_creates_a_distinct_message() {
        let f = Fixture::new();
        let old = f
            .store
            .apply("inbound", auto_part("old-1", 1, 2, 10_000, "old "))
            .unwrap();
        f.store
            .apply("inbound", auto_part("old-2", 2, 2, 11_000, "message"))
            .unwrap();
        let next = f
            .store
            .apply("inbound", auto_part("new-1", 1, 2, 12_000, "new "))
            .unwrap();
        assert_ne!(old["message_id"], next["message_id"]);
        f.store
            .apply("inbound", auto_part("new-2", 2, 2, 13_000, "message"))
            .unwrap();
        let messages = f.store.read("messages", Some("A"), 0).unwrap();
        assert_eq!(messages["messages"].as_array().unwrap().len(), 2);
        assert_eq!(messages["messages"][0]["body"], "old message");
        assert_eq!(messages["messages"][1]["body"], "new message");
    }

    #[test]
    fn auto_multipart_conflicting_index_preserves_raw_fragments_without_notification() {
        let f = Fixture::new();
        f.store
            .apply("inbound", auto_part("collision-1", 1, 2, 10_000, "first "))
            .unwrap();
        let conflict = f
            .store
            .apply(
                "inbound",
                auto_part("collision-2", 1, 2, 11_000, "different "),
            )
            .unwrap();
        assert_eq!(conflict["state"], "undecoded");
        let reloaded = Store::open(&f.root).unwrap();
        reloaded.recover().unwrap();
        assert_eq!(
            reloaded
                .apply("inbound", auto_part("collision-tail", 2, 2, 12_000, "tail"))
                .unwrap()["state"],
            "undecoded"
        );
        assert!(
            reloaded.read("messages", Some("A"), 0).unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            reloaded.read("changes", None, 0).unwrap()["changes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let state = read_snapshot(&reloaded.0.load().unwrap()).unwrap();
        let group = state
            .messages
            .values()
            .find(|m| m["state"] == "ambiguous")
            .unwrap();
        assert!(group["parts"]["1"]["raw_tpdu_hex"].as_str().is_some());
        assert_eq!(
            state
                .messages
                .values()
                .filter(|m| m["state"] == "undecoded")
                .count(),
            2
        );
    }

    #[test]
    fn auto_multipart_multiple_window_candidates_are_quarantined() {
        let f = Fixture::new();
        f.store
            .apply("inbound", auto_part("window-1", 1, 3, 10_000, "one "))
            .unwrap();
        f.store
            .apply("inbound", auto_part("window-2", 1, 3, 210_000, "two "))
            .unwrap();
        // Out-of-order SCTS falls within both independent incomplete windows.
        let reply = f
            .store
            .apply(
                "inbound",
                auto_part("window-middle", 2, 3, 110_000, "unknown "),
            )
            .unwrap();
        assert_eq!(reply["state"], "undecoded");
        let state = read_snapshot(&f.store.0.load().unwrap()).unwrap();
        assert_eq!(
            state
                .messages
                .values()
                .filter(|m| m["state"] == "ambiguous")
                .count(),
            2
        );
        assert!(
            f.store.read("changes", None, 0).unwrap()["changes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn auto_multipart_profile_reference_width_and_time_window_do_not_mix() {
        let f = Fixture::new();
        let first = f
            .store
            .apply("inbound", auto_part("scope-first", 1, 2, 10_000, "A "))
            .unwrap();
        let mut other_profile = auto_part("scope-B", 2, 2, 11_000, "B");
        other_profile["profile_id"] = json!("B");
        assert_eq!(
            f.store.apply("inbound", other_profile).unwrap()["state"],
            "partial"
        );
        let mut other_width = auto_part("scope-width", 2, 2, 11_000, "16");
        other_width["part"]["reference"] = json!("16:42");
        assert_eq!(
            f.store.apply("inbound", other_width).unwrap()["state"],
            "partial"
        );
        let later = f
            .store
            .apply("inbound", auto_part("scope-later", 2, 2, 130_001, "late"))
            .unwrap();
        assert_eq!(later["state"], "partial");
        assert_ne!(later["message_id"], first["message_id"]);
        assert!(
            f.store.read("messages", Some("A"), 0).unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn recovery_selection_is_live_sorted_and_limited_to_first_live_card() {
        let f = Fixture::new();
        f.store
            .apply(
                "catalog",
                json!({"event_id":"catalog-card-b","profiles":[
                    {"profile_id":"C","card_id":"card-B","eid":"E2","iccid":"3"},
                    {"profile_id":"D","card_id":"card-B","eid":"E2","iccid":"4"}
                ]}),
            )
            .unwrap();
        for p in ["C", "D"] {
            f.store
                .apply(
                    "desired",
                    json!({"event_id":format!("start-{p}"),"profile_id":p,"desired_state":"keep_online"}),
                )
                .unwrap();
        }
        f.store
            .apply(
                "catalog",
                json!({"event_id":"tombstone-D","profiles":[
                    {"profile_id":"D","card_id":"card-B","tombstone":true}
                ]}),
            )
            .unwrap();

        let all_live = f
            .store
            .recovery_selection(&["A", "B", "C", "D"].map(str::to_owned))
            .unwrap();
        assert_eq!(all_live["profile_ids"], json!(["A", "B"]));
        assert_eq!(all_live["card_id"], "C");

        let card_b_only = f
            .store
            .recovery_selection(&["C", "D"].map(str::to_owned))
            .unwrap();
        assert_eq!(card_b_only["profile_ids"], json!(["C"]));
        assert_eq!(card_b_only["card_id"], "card-B");

        let none_live = f.store.recovery_selection(&[]).unwrap();
        assert_eq!(none_live["profile_ids"], json!([]));
        assert_eq!(none_live["card_id"], "");
    }

    #[test]
    fn typed_recovery_selection_is_durable_filtered_and_read_only() {
        let f = Fixture::new();
        let before = f.store.read("changes", None, 0).unwrap();
        let reopened = Store::open(&f.root).unwrap();
        let selection = reopened
            .recovery_selection(&["A".into(), "B".into()])
            .unwrap();
        assert_eq!(selection["profile_ids"], json!(["A", "B"]));
        assert_eq!(selection["card_id"], "C");
        let filtered = reopened
            .recovery_selection(&["A".into(), "A".into(), "unknown".into()])
            .unwrap();
        assert_eq!(filtered["profile_ids"], json!(["A"]));
        assert_eq!(
            reopened.recovery_selection(&[]).unwrap()["profile_ids"],
            json!([])
        );
        assert_eq!(reopened.read("changes", None, 0).unwrap(), before);
    }

    #[test]
    fn typed_claim_next_claims_once_and_recovery_never_reauthorizes_submission() {
        let f = Fixture::new();
        f.queue("A", "private-claim-request");
        let body = json!({
            "event_id":"private-claim-event",
            "profile_id":"A",
            "attempt_token":"private-claim-attempt"
        });
        let value = f.store.apply("claim-next", body.clone()).unwrap();
        assert_eq!(value["message"]["state"], "submitting");
        assert_eq!(f.store.apply("claim-next", body.clone()).unwrap(), value);
        let reopened = Store::open(&f.root).unwrap();
        reopened.recover().unwrap();
        assert!(reopened.apply("claim-next", body).is_err());
        let poll = reopened
            .apply(
                "claim-next",
                json!({
                    "event_id":"after-recovery", "profile_id":"A", "attempt_token":"new-attempt"
                }),
            )
            .unwrap();
        assert!(poll["message"].is_null());
        assert_eq!(
            reopened.read("messages", Some("A"), 0).unwrap()["messages"][0]["state"],
            "unknown"
        );
    }

    #[test]
    fn outbox_idempotence_cancel_fence_and_uncertain_crash_never_resubmit() {
        let f = Fixture::new();
        let queued = f.queue("A", "request");
        let id = queued["message_id"].as_str().unwrap();
        assert_eq!(f.queue("A", "request"), queued);
        assert!(f.store.apply("queue",json!({"event_id":"different-body","profile_id":"A","client_request_id":"request","phone":"+1000","body":"changed"})).is_err());
        let desired = f.store.read("catalog", None, 0).unwrap()["desired"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["profile_id"] == "A")
            .unwrap()["revision"]
            .as_u64()
            .unwrap();
        f.store
            .apply(
                "desired",
                json!({"event_id":"stop-A","profile_id":"A","desired_state":"stopped"}),
            )
            .unwrap();
        assert!(f.store.apply("claim",json!({"event_id":"claim-stopped","profile_id":"A","message_id":id,"attempt_token":"attempt","expected_revision":desired})).is_err());
        f.store
            .apply(
                "desired",
                json!({"event_id":"restart-A","profile_id":"A","desired_state":"keep_online"}),
            )
            .unwrap();
        assert!(f.store.apply("claim",json!({"event_id":"claim-stale","profile_id":"A","message_id":id,"attempt_token":"attempt","expected_revision":desired})).is_err());
        f.store.apply("claim",json!({"event_id":"claim","profile_id":"A","message_id":id,"attempt_token":"attempt"})).unwrap();
        let reloaded = Store::open(&f.root).unwrap();
        reloaded.recover().unwrap();
        assert!(
            reloaded.read("pending", None, 0).unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reloaded.read("messages", Some("A"), 0).unwrap()["messages"][0]["state"],
            "unknown"
        );
        assert!(reloaded.apply("claim",json!({"event_id":"reclaim","profile_id":"A","message_id":id,"attempt_token":"attempt2"})).is_err());
        assert!(reloaded.apply("outcome",json!({"event_id":"wrong-owner","profile_id":"A","message_id":id,"attempt_token":"wrong","state":"delivered"})).is_err());
        reloaded.apply("outcome",json!({"event_id":"late-valid","profile_id":"A","message_id":id,"attempt_token":"attempt","state":"delivered"})).unwrap();
    }

    #[test]
    fn claim_next_atomically_selects_in_existing_order_and_replays_only_live_claim() {
        let f = Fixture::new();
        f.queue("A", "first-request");
        f.queue("A", "second-request");
        let expected =
            f.store.read("pending", Some("A"), 0).unwrap()["messages"][0]["message_id"].clone();

        let claim = json!({
            "event_id":"claim-next-one",
            "profile_id":"A",
            "attempt_token":"attempt-one"
        });
        let first = f.store.apply("claim-next", claim.clone()).unwrap();
        assert_eq!(first["message"]["message_id"], expected);
        assert_eq!(first["message"]["state"], "submitting");
        assert_eq!(f.store.apply("claim-next", claim.clone()).unwrap(), first);
        f.store
            .apply(
                "desired",
                json!({"event_id":"claim-next-stop","profile_id":"A","desired_state":"stopped"}),
            )
            .unwrap();
        f.store
            .apply(
                "desired",
                json!({"event_id":"claim-next-resume","profile_id":"A","desired_state":"keep_online"}),
            )
            .unwrap();
        assert!(
            f.store.apply("claim-next", claim).is_err(),
            "a changed desired-state generation cannot replay an old send authorization"
        );

        let second = f
            .store
            .apply(
                "claim-next",
                json!({
                    "event_id":"claim-next-two",
                    "profile_id":"A",
                    "attempt_token":"attempt-two"
                }),
            )
            .unwrap();
        assert_ne!(second["message"]["message_id"], expected);
        assert_eq!(second["message"]["state"], "submitting");

        f.store
            .apply(
                "outcome",
                json!({
                    "event_id":"claim-next-outcome",
                    "profile_id":"A",
                    "message_id":first["message"]["message_id"],
                    "attempt_token":"attempt-one",
                    "state":"accepted"
                }),
            )
            .unwrap();
        assert!(
            f.store
                .apply(
                    "claim-next",
                    json!({
                        "event_id":"claim-next-one",
                        "profile_id":"A",
                        "attempt_token":"attempt-one"
                    })
                )
                .is_err(),
            "a stale retry cannot authorize a second network submission"
        );
    }

    #[test]
    fn claim_next_empty_poll_is_read_only_and_concurrent_claims_are_unique() {
        let f = Fixture::new();
        let empty = f
            .store
            .apply(
                "claim-next",
                json!({
                    "event_id":"empty-poll",
                    "profile_id":"A",
                    "attempt_token":"empty-attempt"
                }),
            )
            .unwrap();
        assert!(empty["message"].is_null());
        assert!(f
            .store
            .apply(
                "claim-next",
                json!({"event_id":"invalid-claim-next","profile_id":"A","attempt_token":"invalid-attempt","unexpected":true}),
            )
            .is_err(), "the private action rejects unreviewed fields");

        f.queue("A", "queued-one");
        f.queue("A", "queued-two");
        let store = Arc::new(f.store.clone());
        let workers: Vec<_> = (0..8)
            .map(|index| {
                let store = store.clone();
                std::thread::spawn(move || {
                    store
                        .apply(
                            "claim-next",
                            json!({
                                "event_id":format!("parallel-claim-{index}"),
                                "profile_id":"A",
                                "attempt_token":format!("parallel-attempt-{index}")
                            }),
                        )
                        .unwrap()
                })
            })
            .collect();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        let ids: HashSet<_> = results
            .iter()
            .filter_map(|result| result["message"]["message_id"].as_str())
            .collect();
        assert_eq!(
            ids.len(),
            2,
            "each queued item is claimed by at most one worker"
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| result["message"].is_null())
                .count(),
            6
        );
    }

    #[test]
    fn concurrent_duplicate_submission_has_one_message_and_payload_conflict_is_rejected() {
        let f = Fixture::new();
        let store = Arc::new(f.store.clone());
        let workers:Vec<_>=(0..8).map(|i|{let store=store.clone();std::thread::spawn(move||store.apply("queue",json!({"event_id":format!("e{i}"),"profile_id":"A","client_request_id":"same","phone":"+1000","body":"hello"})).unwrap())}).collect();
        let results: Vec<_> = workers.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(
            results
                .iter()
                .all(|r| r["message_id"] == results[0]["message_id"])
        );
        assert_eq!(
            store.read("pending", None, 0).unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .apply(
                    "desired",
                    json!({"event_id":"e0","profile_id":"A","desired_state":"stopped"})
                )
                .is_err()
        );
    }

    #[test]
    fn failed_write_never_reports_durable_success_or_truncates_existing_records() {
        let f = Fixture::new();
        let before = std::fs::read(f.store.0.path()).unwrap();
        let limited = Store(
            ArchiveStore::with_limits(f.store.0.path(), before.len() as u64 + 10, 1)
                .unwrap()
                .into(),
        );
        let body = "x".repeat(2048);
        assert!(
            limited
                .apply("inbound", incoming("A", "too-large", "large", "new", &body))
                .is_err()
        );
        assert_eq!(std::fs::read(f.store.0.path()).unwrap(), before);
    }

    #[test]
    fn application_credential_cannot_authorize_internal_store_or_reservation() {
        let app = Request::builder()
            .header("x-proletarius-app-token", "app")
            .body(())
            .unwrap();
        assert!(!internal_authenticated(&app, "private"));
        let valid = Request::builder()
            .header(INTERNAL_HEADER, "private")
            .body(())
            .unwrap();
        assert!(internal_authenticated(&valid, "private"));
        assert!(!internal_authenticated(&valid, "other"));
        assert!(!internal_authenticated(&valid, ""));
    }
    #[test]
    fn cached_claim_cannot_authorize_network_replay_after_recovery() {
        let f = Fixture::new();
        let queued = f.queue("A", "crash-test");
        let claim = json!({"event_id":"claim-once","profile_id":"A","message_id":queued["message_id"],"attempt_token":"attempt"});
        f.store.apply("claim", claim.clone()).unwrap();
        f.store.recover().unwrap();
        assert!(f.store.apply("claim", claim).is_err());
    }

    #[test]
    fn undecodable_sms_is_durable_and_never_published_as_empty_text() {
        let f = Fixture::new();
        let reply=f.store.apply("inbound",json!({"event_id":"bad-decode","dedup_key":"wire-undecoded","profile_id":"A","message_id":"opaque","kind":"undecoded","raw_tpdu_hex":"ffaa","report":{"decode_error":"unsupported_dcs"}})).unwrap();
        assert_eq!(reply["state"], "undecoded");
        assert_eq!(reply["complete"], false);
        assert!(
            f.store.read("changes", None, 0).unwrap()["changes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            read_snapshot(&f.store.0.load().unwrap())
                .unwrap()
                .messages
                .values()
                .filter(|m| m["kind"] == "undecoded")
                .count(),
            1
        );
    }

    async fn http_roundtrip(
        store: Store,
        method: &str,
        path: &str,
        header: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let private_socket = path.starts_with("/api/internal/");
        http_roundtrip_on_socket(store, method, path, header, token, body, private_socket).await
    }

    async fn http_roundtrip_on_socket(
        store: Store,
        method: &str,
        path: &str,
        header: &str,
        token: &str,
        body: Value,
        private_socket: bool,
    ) -> (StatusCode, Value) {
        let store_root = store.0.path().parent().unwrap().to_owned();
        let operation_root = store_root.parent().unwrap_or(&store_root).to_owned();
        let sms_inbox = crate::message_archive::InboxState::default();
        let state = crate::ServerState {
            call_mode: Default::default(),
            module_call: Default::default(),
            reader_lifecycle: Default::default(),
            multistandby_card: Default::default(),
            multistandby_service: Default::default(),
            app_token: Some("app-fixture".into()),
            internal_token: "private-fixture".into(),
            multistandby_operations: crate::multistandby_operations::Store::open(&operation_root)
                .unwrap(),
            device_send_store: crate::device_send_store::Store::open(
                store.0.path().parent().unwrap(),
            )
            .unwrap(),
            multistandby_store: store,
            forwarding_handoff_store: crate::forwarding_handoff_store::Store::open(&store_root)
                .unwrap(),
            sms_forwarding: crate::sms_forwarding::Service::default(),
            sms_inbox: sms_inbox.clone(),
            usb_sms_receive: crate::test_usb_sms_receiver(
                sms_inbox.clone(),
                store_root.join("sms-archive.json"),
            ),
            call_archive: proletarius_store::ArchiveStore::new(store_root.join("call-log.json")),
            call_facts: crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(store_root.join("call-log.json")),
                "call-producer-fixture".to_owned(),
            ),
            go_socket_path: std::path::PathBuf::new(),
            availability: crate::BackendAvailability::new(),
            reader_discovery: Arc::new(std::sync::Mutex::new(crate::ReaderDiscovery::default())),
            snapshot_instance_id: "fixture".into(),
            snapshot_revision: Arc::new(crate::AtomicU64::new(0)),
            eap_exchanges: Arc::new(crate::eap_identity::EapExchangeStore::default()),
            reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
            reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
            modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
            qmi_enhancement: crate::qmi_enhancement::QmiEnhancement::default(),
            network_traffic: crate::network_traffic::Service::default(),
            usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
            module_radio: crate::test_module_radio(&store_root),
            native_vowifi_lifecycle: std::sync::Arc::new(
                crate::native_vowifi_session::NativeLifecycle::default(),
            ),
            ims_sms_transactions: crate::ims_sms_transactions::Service::default(),
            multi_runtime_report: std::sync::Arc::new(std::sync::Mutex::new(
                crate::multistandby_runtime_report::Journal::default(),
            )),
        };
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let server = tokio::spawn(async move {
            if private_socket {
                crate::serve_internal_connection(server, state).await
            } else {
                crate::serve_connection(server, state).await
            }
        });
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(client))
                .await
                .unwrap();
        let connection = tokio::spawn(connection);
        let mut request = Request::builder().method(method).uri(path);
        if !header.is_empty() {
            request = request.header(header, token);
        }
        let request = request
            .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
            .unwrap();
        let response = sender.send_request(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        drop(sender);
        if let Err(error) = connection.await.unwrap() {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
        if let Err(error) = server.await.unwrap() {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn send_intent_native_http_requires_app_auth_and_queries_current_profile_scoped_evidence()
    {
        let f = Fixture::new();
        let base = "/api/multistandby/send-intents";
        let body = send_intent_body("http-intent");
        assert_eq!(
            http_roundtrip(
                f.store.clone(),
                "POST",
                base,
                "x-proletarius-app-token",
                "wrong",
                body.clone()
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        let (status, prepared) = http_roundtrip(
            f.store.clone(),
            "POST",
            base,
            "x-proletarius-app-token",
            "app-fixture",
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(prepared["state"], "prepared");
        let path = format!("{base}?profile_id=A&client_request_id=http-intent");
        assert_eq!(
            http_roundtrip(
                f.store.clone(),
                "GET",
                &path,
                "x-proletarius-app-token",
                "wrong",
                Value::Null
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            http_roundtrip(
                f.store.clone(),
                "GET",
                &path,
                "x-proletarius-app-token",
                "app-fixture",
                Value::Null
            )
            .await
            .1["state"],
            "prepared"
        );
        let submit = format!("{base}/submit");
        assert_eq!(
            http_roundtrip(
                f.store.clone(),
                "POST",
                &submit,
                "x-proletarius-app-token",
                "app-fixture",
                body.clone()
            )
            .await
            .1["can_submit"],
            true
        );
        assert_eq!(
            http_roundtrip(
                f.store.clone(),
                "POST",
                &submit,
                "x-proletarius-app-token",
                "app-fixture",
                body.clone()
            )
            .await
            .1["can_submit"],
            false
        );
        for invalid in [
            format!("{base}?client_request_id=http-intent"),
            format!("{base}?profile_id=A"),
            format!("{base}?profile_id=A&client_request_id=http-intent&cursor=bad"),
            format!("{base}?peer_hex=00"),
        ] {
            assert_eq!(
                http_roundtrip(
                    f.store.clone(),
                    "GET",
                    &invalid,
                    "x-proletarius-app-token",
                    "app-fixture",
                    Value::Null
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            http_roundtrip(
                f.store.clone(),
                "GET",
                &format!("{base}?profile_id=B&client_request_id=http-intent"),
                "x-proletarius-app-token",
                "app-fixture",
                Value::Null
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        let queued = f.queue("A", "http-intent");
        f.store.apply("claim",json!({"event_id":"http-intent-claim","profile_id":"A","message_id":queued["message_id"],"attempt_token":"private-attempt"})).unwrap();
        f.store.apply("outcome",json!({"event_id":"http-intent-accepted","profile_id":"A","message_id":queued["message_id"],"attempt_token":"private-attempt","state":"accepted"})).unwrap();
        let result = http_roundtrip(
            f.store.clone(),
            "GET",
            &path,
            "x-proletarius-app-token",
            "app-fixture",
            Value::Null,
        )
        .await
        .1;
        assert_eq!(result["state"], "accepted");
        assert_ne!(result["revision"], queued["revision"]);
        assert_eq!(result["message_id"], queued["message_id"]);
        assert!(!result.to_string().contains("private-attempt"));
    }

    #[tokio::test]
    async fn typed_inbound_and_public_history_preserve_active_private_credential_boundaries() {
        let f = Fixture::new();
        // Store and reader operation IPC retired. The remaining module busy
        // observer requires the full private credential before validating input.
        let path = "/api/internal/reader-business";
        for (header, token) in [
            ("x-proletarius-app-token", "app-fixture"),
            (INTERNAL_HEADER, "private-fixture"),
        ] {
            let (status, body) = http_roundtrip_on_socket(
                f.store.clone(),
                "POST",
                path,
                header,
                token,
                json!({}),
                false,
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(body["code"], "route_not_found");
        }
        for (header, token, expected) in [
            ("", "", StatusCode::UNAUTHORIZED),
            (
                "x-proletarius-app-token",
                "app-fixture",
                StatusCode::UNAUTHORIZED,
            ),
            (INTERNAL_HEADER, "wrong-token", StatusCode::UNAUTHORIZED),
            (INTERNAL_HEADER, "private-fixture", StatusCode::BAD_REQUEST),
        ] {
            assert_eq!(
                http_roundtrip(f.store.clone(), "POST", path, header, token, json!({}))
                    .await
                    .0,
                expected
            );
        }
        f.store
            .apply(
                "inbound",
                incoming("A", "http-in", "http-wire", "http-message", "new text"),
            )
            .unwrap();
        let (status, body) = http_roundtrip(
            f.store.clone(),
            "GET",
            "/api/multistandby/messages?profile_id=A&cursor=0",
            "x-proletarius-app-token",
            "app-fixture",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["messages"][0]["body"], "new text");
        assert!(body["messages"][0].get("parts").is_none());
        let before = f.store.read("changes", None, 0).unwrap();
        for route in [
            "/api/multistandby/history?profile_id=A&peer_hex=2b31303030",
            "/api/multistandby/conversations?profile_id=A",
        ] {
            let (status, body) = http_roundtrip(
                f.store.clone(),
                "GET",
                route,
                "x-proletarius-app-token",
                "app-fixture",
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            if route.contains("/history") {
                assert_eq!(body["messages"][0]["message_id"], "http-message");
            } else {
                assert_eq!(body["conversations"][0]["message_count"], 1);
            }
            assert_eq!(
                http_roundtrip(
                    f.store.clone(),
                    "GET",
                    route,
                    INTERNAL_HEADER,
                    "private-fixture",
                    Value::Null
                )
                .await
                .0,
                StatusCode::UNAUTHORIZED
            );
        }
        for route in [
            "/api/multistandby/history?profile_id=A&peer_hex=1",
            "/api/multistandby/history?profile_id=A&cursor=invalid",
            "/api/multistandby/history?profile_id=A&conversation_id=invalid",
            "/api/multistandby/conversations?peer_hex=2b31303030",
        ] {
            assert_eq!(
                http_roundtrip(
                    f.store.clone(),
                    "GET",
                    route,
                    "x-proletarius-app-token",
                    "app-fixture",
                    Value::Null
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(f.store.read("changes", None, 0).unwrap(), before);
        assert_eq!(
            http_roundtrip(
                f.store.clone(),
                "GET",
                "/api/multistandby/messages?profile_id=A",
                INTERNAL_HEADER,
                "private-fixture",
                Value::Null
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn typed_catalog_discovery_persists_rust_owned_ids_across_reopen() {
        let f = Fixture::new();
        let body = json!({
            "event_id":"discover-native-1", "reader":"reader-native-A",
            "profiles":[{"eid":"eid-native-A","iccid":"iccid-native-A"}]
        });
        let response = f.store.apply("catalog", body.clone()).unwrap();
        assert!(
            response["profiles"][0]["profile_id"]
                .as_str()
                .unwrap()
                .starts_with("profile_")
        );
        assert!(
            response["profiles"][0]["card_id"]
                .as_str()
                .unwrap()
                .starts_with("card_")
        );
        let reopened = Store::open(&f.root).unwrap();
        assert_eq!(reopened.apply("catalog", body).unwrap(), response);
        assert!(
            reopened.runtime_catalog().unwrap()["profiles"]
                .as_array()
                .unwrap()
                .iter()
                .any(
                    |profile| profile["profile_id"] == response["profiles"][0]["profile_id"]
                        && profile["card_id"] == response["profiles"][0]["card_id"]
                )
        );
    }

    #[test]
    fn typed_inbound_event_persists_normalized_message_and_raw_part_once() {
        let f = Fixture::new();
        let mut event = inbound_event("A", "native-event", "010203", "network text", 1234, 1);
        event.as_object_mut().unwrap().remove("concat");
        let response = f.store.apply("inbound-event", event.clone()).unwrap();
        assert_eq!(response["state"], "received");
        let reopened = Store::open(&f.root).unwrap();
        assert_eq!(reopened.apply("inbound-event", event).unwrap(), response);
        let messages = reopened.read("messages", Some("A"), 0).unwrap();
        assert_eq!(messages["messages"].as_array().unwrap().len(), 1);
        assert_eq!(messages["messages"][0]["body"], "network text");
        let snapshot = read_snapshot(&reopened.0.load().unwrap()).unwrap();
        let stored = snapshot
            .messages
            .values()
            .find(|message| message["body"] == "network text")
            .unwrap();
        assert_eq!(stored["parts"]["1"]["raw_rpdu_hex"], "010203");
    }

    #[test]
    fn rust_profile_projection_merges_live_and_durable_offline_profiles_compatibly() {
        let runtime = json!({
            "boot_id":"boot-fixture",
            "profiles":[
                {"profile_id":"live-A","card_id":"card-A","eid":"eid-A","iccid":"iccid-live","display_name":"Live","phone_number":"+1000","physical_enabled":true,"desired_state":"keep_online","auth_state":"ready","tunnel_state":"connected","ims_state":"registered","sms_ready":true,"health":"ready","last_error":"","session_generation":3,"consecutive_failures":0}
            ]
        });
        let catalog = json!({
            "revision":19,
            "profiles":[
                {"profile_id":"live-A","card_id":"card-A","eid":"eid-A","iccid":"iccid-live","display_name":"Live","phone_number":"+1000","tombstone":false},
                {"profile_id":"offline-A","card_id":"card-A","eid":"eid-A","iccid":"iccid-offline","display_name":"Offline","phone_number":"","tombstone":false},
                {"profile_id":"other-card","card_id":"card-B","eid":"eid-B","iccid":"iccid-other","display_name":"Other","phone_number":"","tombstone":false}
            ],
            "desired":[
                {"profile_id":"offline-A","desired_state":"keep_online"},
                {"profile_id":"other-card","desired_state":"stopped"}
            ]
        });

        let all = project_profiles(&runtime, &catalog, "").unwrap();
        assert_eq!(all["boot_id"], "boot-fixture");
        assert_eq!(all["revision"], 19);
        assert_eq!(all["profiles"].as_array().unwrap().len(), 3);
        assert_eq!(all["profiles"][0]["health"], "ready");
        let offline = &all["profiles"][1];
        assert_eq!(offline["profile_id"], "offline-A");
        assert_eq!(offline["health"], "offline");
        assert_eq!(offline["desired_state"], "keep_online");
        assert_eq!(offline["last_error"], "卡片当前不可用；历史短信仍可查看");
        assert_eq!(offline["physical_enabled"], false);
        assert_eq!(offline["sms_ready"], false);
        assert_eq!(offline["session_generation"], 0);
        assert_eq!(offline["consecutive_failures"], 0);
        assert!(offline.get("tombstone").is_none());

        let card = project_profiles(&runtime, &catalog, "card-A").unwrap();
        assert_eq!(card["profiles"].as_array().unwrap().len(), 2);
        assert!(
            card["profiles"]
                .as_array()
                .unwrap()
                .iter()
                .all(|profile| { profile["card_id"] == "card-A" })
        );
        assert_eq!(profile_card_filter("ignored=x&card_id=card%2DA"), "card-A");
        assert_eq!(profile_card_filter("ignored=x"), "");
    }

    #[test]
    fn rust_card_projection_groups_profiles_and_preserves_discovery_status() {
        let runtime = json!({
            "boot_id":"boot-fixture",
            "available":false,
            "detail":"读卡器未初始化",
            "card_profiles":[
                {"card_id":"card-A","reader":"reader-A","eid":"eid-A"},
                {"card_id":"card-A","reader":"reader-A","eid":"eid-A"},
                {"card_id":"card-B","reader":"reader-B","eid":"eid-B"}
            ]
        });

        let cards = project_cards(&runtime).unwrap();
        assert_eq!(cards["boot_id"], "boot-fixture");
        assert_eq!(cards["detail"], "读卡器未初始化");
        assert_eq!(cards["cards"].as_array().unwrap().len(), 2);
        assert_eq!(cards["cards"][0]["card_id"], "card-A");
        assert_eq!(cards["cards"][0]["profile_count"], 2);
        assert_eq!(cards["cards"][0]["reader"], "reader-A");
        assert_eq!(cards["cards"][0]["available"], false);
        assert_eq!(cards["cards"][1]["card_id"], "card-B");
        assert_eq!(cards["cards"][1]["profile_count"], 1);
    }
}
