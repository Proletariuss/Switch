//! Durable at-most-once dispatch permission. This ledger binds a payload to an
//! opaque Go process/route reservation; it does not assert physical SIM identity.
mod authority_v2;

use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) const PREFIX: &str = "/api/internal/device-send/";
const MAX_RECORDS: usize = 10_000;
const MAX_BYTES: u64 = 32 * 1024 * 1024;
#[derive(Clone)]
pub(crate) struct Store(
    crate::sms_store_health::GuardedArchive,
    crate::device_sms_send::owner::Owner,
    Option<Arc<authority_v2::seal_adapter::Adapter>>,
);
/// Constructed only while open_with_authority owns an unpublished fresh Owner.
pub(crate) struct StartupMigration {
    proof: crate::daemon_lifetime::RootProof,
}
impl StartupMigration {
    pub(in crate::device_send_store) fn validate(&self, proof: &crate::daemon_lifetime::RootProof) -> bool {
        self.proof.same_authority(proof)
    }
}
type Adapter = authority_v2::seal_adapter::Adapter;
static AUTHORITIES: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Adapter>>>> = OnceLock::new();
fn registered(root: &Path) -> Result<Option<Arc<Adapter>>, StoreError> {
    let mut registry = AUTHORITIES.get_or_init(Default::default).lock().map_err(|_| error("sms_store_unavailable"))?;
    registry.retain(|_, authority| authority.strong_count() > 0);
    Ok(registry.get(root).and_then(Weak::upgrade))
}
fn codec_error(error: authority_v2::CodecError) -> StoreError {
    use authority_v2::CodecError::*;
    error_code(match error {
        InvalidJson | InvalidReserve | InvalidIdentity => "invalid_request",
        Conflict | LegacyCollision => "send_intent_conflict",
        Retired => "mutation_identity_retired",
        NotIssued => "mutation_identity_not_issued",
        NotFound => "send_intent_not_found",
        UpgradeRequired => "mutation_identity_upgrade_required",
        AuthorityMismatch => "mutation_authority_mismatch",
        SequenceExhausted => "mutation_sequence_exhausted",
        Capacity => "ledger_full",
        InvalidSnapshot | LegacyUnavailable => "sms_store_unavailable",
    })
}
fn error_code(code: &str) -> StoreError { StoreError::Configuration(code.into()) }

#[derive(Clone, Deserialize, Serialize)]
struct Record {
    client_request_id: String,
    phone: String,
    message: String,
    device: String,
    #[serde(default)]
    subscription_id: String,
    binding: String,
    state: String,
    attempt: String,
    segments: usize,
    archive_confirmed: bool,
    created_at_ms: u64,
    updated_at_ms: u64,
}
fn error(code: &str) -> StoreError {
    StoreError::Configuration(code.into())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
fn token(body: &Value, name: &str) -> Result<String, StoreError> {
    let s = body[name]
        .as_str()
        .ok_or_else(|| error("invalid_request"))?;
    if !valid_token(s) {
        return Err(error("invalid_request"));
    }
    Ok(s.into())
}
fn text(body: &Value, name: &str, max: usize) -> Result<String, StoreError> {
    let s = body[name]
        .as_str()
        .ok_or_else(|| error("invalid_request"))?;
    if s.trim().is_empty() || s.len() > max {
        return Err(error("invalid_request"));
    }
    Ok(s.into())
}
fn read(items: &[Value], existed: bool) -> Result<BTreeMap<String, Record>, StoreError> {
    if items.is_empty() {
        if existed {
            return Err(error("ledger_corrupt"));
        }
        return Ok(BTreeMap::new());
    }
    if items.len() != 1 || items[0]["schema_version"] != 1 {
        return Err(error("ledger_corrupt"));
    }
    let records: BTreeMap<String, Record> =
        serde_json::from_value(items[0]["records"].clone()).map_err(StoreError::Json)?;
    if records.len() > MAX_RECORDS
        || records.iter().any(|(key, r)| {
            key != &r.client_request_id
                || !valid_token(key)
                || !valid_token(&r.binding)
                || !matches!(r.device.as_str(), "reader" | "dji")
                || r.phone.trim().is_empty()
                || r.phone.len() > 128
                || r.message.trim().is_empty()
                || r.message.len() > 64 * 1024
                || (!r.subscription_id.is_empty() && !valid_token(&r.subscription_id))
                || match r.state.as_str() {
                    "prepared" => !r.attempt.is_empty() || r.segments != 0 || r.archive_confirmed,
                    "submitting" => {
                        !valid_token(&r.attempt) || r.segments != 0 || r.archive_confirmed
                    }
                    "unknown" => {
                        !valid_token(&r.attempt) || r.segments > 1024 || r.archive_confirmed
                    }
                    "accepted" => !valid_token(&r.attempt) || !(1..=1024).contains(&r.segments),
                    _ => true,
                }
        })
    {
        return Err(error("ledger_corrupt"));
    }
    Ok(records)
}
fn write(items: &mut Vec<Value>, records: BTreeMap<String, Record>) {
    *items = vec![json!({"schema_version":1,"records":records})];
}
fn record_value(record: &Record, can_submit: bool) -> Value {
    let mut v = serde_json::to_value(record).expect("record serialization");
    v["can_submit"] = json!(can_submit);
    v["identity_version"] = json!(1);
    v
}
impl Store {
    pub(crate) fn strict_v2_body(bytes: &[u8]) -> Result<Value, StoreError> {
        authority_v2::strict_value(bytes, 1024 * 1024).map_err(codec_error)
    }
    pub(crate) fn reserve_body(bytes: &[u8]) -> Result<Value, StoreError> {
        authority_v2::decode_reserve_bytes(bytes).map_err(codec_error)?;
        Self::strict_v2_body(bytes)
    }
    pub(crate) fn open(root: &Path) -> Result<Self, StoreError> {
        Ok(Self(
            ArchiveStore::with_limits(root.join("device-send-ledger.json"), MAX_BYTES, 1)?.into(),
            Default::default(),
            registered(root)?,
        ))
    }
    pub(crate) fn open_with_authority(
        root: &Path, proof: crate::daemon_lifetime::RootProof,
    ) -> Result<Self, StoreError> {
        let mut store = Self::open(root)?;
        if store.2.is_some() { return Err(error("sms_store_unavailable")); }
        // A virgin root gets a valid empty v1 snapshot under the existing
        // presence-aware writer. Previously missing evidence still rejects.
        if !store.0.path().exists() { store.recover()?; }
        let startup = StartupMigration { proof: proof.clone() };
        let adapter = Arc::new(Adapter::open_or_migrate(root, proof, startup)
            .map_err(|_| error("sms_store_unavailable"))?);
        store.2 = Some(adapter.clone());
        AUTHORITIES.get_or_init(Default::default).lock()
            .map_err(|_| error("sms_store_unavailable"))?
            .insert(root.to_path_buf(), Arc::downgrade(&adapter));
        store.recover()?;
        Ok(store)
    }

    pub(crate) fn is_v2(&self) -> bool { self.2.is_some() }

    pub(crate) fn send_owner(&self) -> crate::device_sms_send::owner::Owner {
        self.1.clone()
    }
    pub(crate) fn status(&self) -> proletarius_api_contract::SmsStoreStatus {
        if self.2.as_ref().is_some_and(|adapter| adapter.restart_required()) {
            return proletarius_api_contract::SmsStoreStatus {
                state: "unavailable", reason: Some("store_io_failed"), restart_required: true,
            };
        }
        self.0.status()
    }
    pub(crate) fn recover(&self) -> Result<(), StoreError> {
        if self.2.is_some() { return self.apply("recover", json!({})).map(|_|()); }
        self.recover_bindings(|_| true)
    }
    pub(crate) fn recover_go_owned(&self) -> Result<(), StoreError> {
        if self.2.is_some() { return self.apply("recover_go", json!({})).map(|_|()); }
        self.recover_bindings(crate::device_sms_send::go_recovery_binding)
    }
    fn recover_bindings(&self, lost_owner: impl Fn(&str) -> bool) -> Result<(), StoreError> {
        self.0.mutate(|items| {
            let mut records = read(items, self.0.path().exists())?;
            for r in records.values_mut() {
                if r.state == "submitting" && lost_owner(&r.binding) {
                    r.state = "unknown".into();
                    r.updated_at_ms = now();
                }
            }
            write(items, records);
            Ok(())
        })
    }
    /// Called while the SMS archive lock is held. A confirmed archive is a
    /// historical receipt (including an explicitly deleted row), not a promise
    /// that the message remains visible. No transition here can grant dispatch.
    pub(crate) fn archive_receipt(
        &self,
        item: &Value,
        expected_attempt: Option<&str>,
        confirm: bool,
    ) -> Result<bool, StoreError> {
        if self.2.is_some() {
            return self.apply("archive_receipt", json!({"item":item,"attempt":expected_attempt,"confirm":confirm}))?
                .as_bool().ok_or_else(||error("ledger_corrupt"));
        }
        let id = token(item, "send_request_id")?;
        self.0.mutate_if_changed(|items| {
            let mut records = read(items, self.0.path().exists())?;
            let r = records.get_mut(&id).ok_or_else(|| error("send_intent_not_found"))?;
            let timestamp = item["timestamp"].as_str()
                .and_then(|value| time::OffsetDateTime::parse(
                    value, &time::format_description::well_known::Rfc3339).ok())
                .map(|value| value.unix_timestamp_nanos());
            if r.state != "accepted"
                || r.attempt.is_empty()
                || expected_attempt.is_some_and(|attempt| attempt != r.attempt)
                || item["direction"] != "out"
                || item["sender"] != r.phone
                || item["content"] != r.message
                || item["device"] != r.device
                || item["subscription_id"].as_str().unwrap_or("") != r.subscription_id
                || timestamp != Some(i128::from(r.created_at_ms) * 1_000_000)
            {
                return Err(error("send_intent_conflict"));
            }
            let pending = !r.archive_confirmed;
            if confirm && pending {
                r.archive_confirmed = true;
                r.updated_at_ms = now();
                write(items, records);
                Ok((pending, true))
            } else {
                Ok((pending, false))
            }
        })
    }
    pub(crate) fn apply(&self, action: &str, body: Value) -> Result<Value, StoreError> {
        if let Some(adapter) = &self.2 {
            return adapter.transact(|active, frozen| authority_v2::production::apply(active, frozen, action, &body))
                .map_err(|failure| match failure {
                    authority_v2::seal_adapter::Error::Rejected(code) => codec_error(code),
                    _ => error("sms_store_unavailable"),
                });
        }
        if action == "list" {
            let cursor = body["cursor"].as_str().unwrap_or("");
            if cursor.len() > 128 {
                return Err(error("invalid_request"));
            }
            let records = self.0.read(|items| read(items, self.0.path().exists()))?;
            let mut selected = records
                .iter()
                .filter(|(id, _)| id.as_str() > cursor)
                .take(101)
                .map(|(_, r)| record_value(r, false))
                .collect::<Vec<_>>();
            let more = selected.len() > 100;
            selected.truncate(100);
            let next = if more {
                selected
                    .last()
                    .and_then(|r| r["client_request_id"].as_str())
                    .map(str::to_owned)
            } else {
                None
            };
            return Ok(json!({"intents":selected,"next_cursor":next}));
        }
        let id = token(&body, "client_request_id")?;
        if action == "get" {
            let records = self.0.read(|items| read(items, self.0.path().exists()))?;
            return records
                .get(&id)
                .map(|r| record_value(r, false))
                .ok_or_else(|| error("send_intent_not_found"));
        }
        self.0.mutate(|items| {
            let mut records = read(items, self.0.path().exists())?;
            let result = match action {
                "prepare" => {
                    let phone = text(&body, "phone", 128)?;
                    let message = text(&body, "message", 64 * 1024)?;
                    let device = token(&body, "device")?;
                    let binding = token(&body, "binding")?;
                    let subscription_id = body["subscription_id"].as_str().unwrap_or("").to_owned();
                    if !subscription_id.is_empty() {
                        token(&body, "subscription_id")?;
                    }
                    if !matches!(device.as_str(), "reader" | "dji") {
                        return Err(error("invalid_request"));
                    }
                    if let Some(r) = records.get(&id) {
                        if r.phone != phone
                            || r.message != message
                            || r.device != device
                            || r.subscription_id != subscription_id
                        {
                            return Err(error("send_intent_conflict"));
                        }
                        record_value(r, false)
                    } else {
                        if records.len() >= MAX_RECORDS {
                            return Err(error("ledger_full"));
                        }
                        let at = now();
                        let r = Record {
                            client_request_id: id.clone(),
                            phone,
                            message,
                            device,
                            subscription_id,
                            binding,
                            state: "prepared".into(),
                            attempt: String::new(),
                            segments: 0,
                            archive_confirmed: false,
                            created_at_ms: at,
                            updated_at_ms: at,
                        };
                        let result = record_value(&r, false);
                        records.insert(id.clone(), r);
                        result
                    }
                }
                "claim" => {
                    let binding = token(&body, "binding")?;
                    let attempt = token(&body, "attempt")?;
                    let r = records
                        .get_mut(&id)
                        .ok_or_else(|| error("send_intent_not_found"))?;
                    if r.binding != binding {
                        return Err(error("send_intent_conflict"));
                    }
                    if r.state == "prepared" {
                        r.state = "submitting".into();
                        r.attempt = attempt;
                        r.updated_at_ms = now();
                        record_value(r, true)
                    } else {
                        record_value(r, false)
                    }
                }
                "finish" => {
                    let attempt = token(&body, "attempt")?;
                    let state = token(&body, "state")?;
                    if !matches!(state.as_str(), "accepted" | "unknown") {
                        return Err(error("invalid_request"));
                    }
                    let segments = body["segments"]
                        .as_u64()
                        .filter(|n| *n <= 1024)
                        .ok_or_else(|| error("invalid_request"))?
                        as usize;
                    let archived = body["archive_confirmed"]
                        .as_bool()
                        .ok_or_else(|| error("invalid_request"))?;
                    if (archived && state != "accepted") || (state == "accepted" && segments == 0) {
                        return Err(error("invalid_request"));
                    }
                    let r = records
                        .get_mut(&id)
                        .ok_or_else(|| error("send_intent_not_found"))?;
                    if r.attempt != attempt || r.attempt.is_empty() {
                        return Err(error("send_intent_conflict"));
                    }
                    if r.state == "submitting" {
                        r.state = state.clone();
                        r.segments = segments;
                        r.archive_confirmed = archived;
                        r.updated_at_ms = now();
                    } else if r.state != state
                        || r.segments != segments
                        || r.archive_confirmed != archived
                    {
                        return Err(error("send_intent_conflict"));
                    }
                    record_value(r, false)
                }
                _ => return Err(error("route_not_found")),
            };
            write(items, records);
            Ok(result)
        })
    }
}
pub(crate) async fn handle(request: Request<Incoming>, store: Store) -> Response<Full<Bytes>> {
    if let Some(response) = crate::sms_store_health::rejection(store.status()) {
        return response;
    }
    let action = request
        .uri()
        .path()
        .strip_prefix(PREFIX)
        .unwrap_or("")
        .to_owned();
    if request.method() != Method::POST
        || !matches!(
            action.as_str(),
            "prepare" | "claim" | "finish" | "get" | "list"
        )
    {
        return crate::json_response(StatusCode::NOT_FOUND, &json!({"code":"route_not_found"}));
    }
    let bytes = match crate::http_server::collect_body(request.into_body(), 128 * 1024).await {
        Ok(bytes) => bytes,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Err(crate::http_server::BodyReadError::TooLarge) => {
            return crate::json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                &json!({"code":"invalid_request"}),
            );
        }
        Err(crate::http_server::BodyReadError::Invalid) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"code":"invalid_request"}),
            );
        }
    };
    let body = match serde_json::from_slice(&bytes) {
        Ok(b) => b,
        Err(_) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"code":"invalid_request"}),
            );
        }
    };
    let health = store.clone();
    match crate::archive_io::run(move || store.apply(&action, body)).await {
        Ok(result) => crate::json_response(StatusCode::OK, &result),
        Err(e) => {
            if let Some(response) = crate::sms_store_health::rejection(health.status()) {
                return response;
            }
            let code = match &e {
                StoreError::Configuration(s) => s.as_str(),
                _ => "store_io_failed",
            };
            let status = match code {
                "invalid_request" => StatusCode::BAD_REQUEST,
                "send_intent_not_found" => StatusCode::NOT_FOUND,
                "send_intent_conflict" => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            crate::json_response(
                status,
                &json!({"error":"设备短信持久记录未确认","code":code,"retryable":false}),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::Arc};
    struct Fixture {
        root: std::path::PathBuf,
        store: Store,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "switch-device-ledger-{}",
                crate::multistandby_store::random_token().unwrap()
            ));
            fs::create_dir_all(&root).unwrap();
            let store = Store::open(&root).unwrap();
            Self { root, store }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn prepare(id: &str) -> Value {
        json!({"client_request_id":id,"phone":"+12025550100","message":"fixture","device":"reader","binding":"route-A","subscription_id":"sub-A"})
    }
    fn claim(id: &str, attempt: &str) -> Value {
        json!({"client_request_id":id,"binding":"route-A","attempt":attempt})
    }
    #[test]
    fn go_exit_recovers_only_dispatcher_owned_bindings_and_native_receipts_still_finish() {
        let f = Fixture::new();
        let cases = [
            ("legacy-reader", "reader", "route-A", true),
            ("legacy-module", "dji", "rust_usb_at:7:route-A", true),
            ("legacy-modem", "dji", "rust_modem:7:route-A", true),
            ("native-reader", "reader", "rust_reader_ims:reader:7", false),
            ("native-module", "dji", "rust_module_ims:module:7", false),
            (
                "native-usb",
                "dji",
                "rust_usb_at_session:session-A:7",
                false,
            ),
            ("native-modem", "dji", "rust_modem_sms_generation:7", false),
            (
                "native-serial-session",
                "dji",
                "rust_modem_session_v2:owner-A:11:7",
                false,
            ),
        ];
        for (id, device, binding, _) in cases {
            let mut request = prepare(id);
            request["device"] = json!(device);
            request["binding"] = json!(binding);
            f.store.apply("prepare", request).unwrap();
            assert_eq!(
                f.store
                    .apply(
                        "claim",
                        json!({"client_request_id":id,"binding":binding,"attempt":"attempt-A"})
                    )
                    .unwrap()["can_submit"],
                true
            );
        }
        f.store.recover_go_owned().unwrap();
        for (id, _, binding, go_owned) in cases {
            let record = f
                .store
                .apply("get", json!({"client_request_id":id}))
                .unwrap();
            assert_eq!(
                record["state"],
                if go_owned { "unknown" } else { "submitting" },
                "{id}"
            );
            assert_eq!(
                f.store
                    .apply(
                        "claim",
                        json!({"client_request_id":id,"binding":binding,"attempt":"attempt-B"})
                    )
                    .unwrap()["can_submit"],
                false
            );
            let result = f.store.apply("finish", json!({"client_request_id":id,"attempt":"attempt-A","state":"accepted","segments":1,"archive_confirmed":true}));
            if go_owned {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap()["state"], "accepted");
            }
        }
        let mut request = prepare("native-restart");
        request["binding"] = json!("rust_reader_ims:reader:8");
        f.store.apply("prepare", request).unwrap();
        f.store.apply("claim", json!({"client_request_id":"native-restart","binding":"rust_reader_ims:reader:8","attempt":"attempt-C"})).unwrap();
        f.store.recover().unwrap();
        assert_eq!(
            f.store
                .apply("get", json!({"client_request_id":"native-restart"}))
                .unwrap()["state"],
            "unknown"
        );
    }
    #[test]
    fn concurrent_claims_grant_exactly_one_persisted_dispatch_and_restart_never_replays() {
        let f = Fixture::new();
        f.store.apply("prepare", prepare("same")).unwrap();
        let store = Arc::new(f.store.clone());
        let workers: Vec<_> = (0..16)
            .map(|i| {
                let s = store.clone();
                std::thread::spawn(move || {
                    s.apply("claim", claim("same", &format!("attempt-{i}")))
                        .unwrap()["can_submit"]
                        .as_bool()
                        .unwrap()
                })
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .filter_map(|w| w.join().unwrap().then_some(()))
                .count(),
            1
        );
        let reloaded = Store::open(&f.root).unwrap();
        reloaded.recover().unwrap();
        assert_eq!(
            reloaded
                .apply("get", json!({"client_request_id":"same"}))
                .unwrap()["state"],
            "unknown"
        );
        assert_eq!(
            reloaded.apply("claim", claim("same", "later")).unwrap()["can_submit"],
            false
        );
    }
    #[test]
    fn payload_subscription_binding_and_late_attempt_conflicts_fail_closed() {
        let f = Fixture::new();
        f.store.apply("prepare", prepare("same")).unwrap();
        for field in ["phone", "message", "device", "subscription_id"] {
            let mut b = prepare("same");
            b[field] = json!(if field == "device" { "dji" } else { "changed" });
            assert!(f.store.apply("prepare", b).is_err(), "{field}");
        }
        let mut wrong = claim("same", "old");
        wrong["binding"] = json!("route-B");
        assert!(f.store.apply("claim", wrong).is_err());
        f.store.apply("claim", claim("same", "current")).unwrap();
        let outcome = json!({"client_request_id":"same","attempt":"current","state":"accepted","segments":1,"archive_confirmed":false});
        let mut late = outcome.clone();
        late["attempt"] = json!("old");
        assert!(f.store.apply("finish", late).is_err());
        assert_eq!(
            f.store.apply("finish", outcome.clone()).unwrap()["state"],
            "accepted"
        );
        assert_eq!(
            f.store.apply("finish", outcome).unwrap()["archive_confirmed"],
            false
        );
        assert_eq!(
            f.store.apply("claim", claim("same", "new")).unwrap()["can_submit"],
            false
        );
    }
    #[test]
    fn persistence_failure_never_grants_permission_and_does_not_change_previous_record() {
        let f = Fixture::new();
        let limited = Store(
            ArchiveStore::with_limits(f.root.join("limited.json"), 64, 1)
                .unwrap()
                .into(),
            Default::default(),
        );
        assert!(limited.apply("prepare", prepare("x")).is_err());
        assert!(!limited.0.path().exists());
        f.store.apply("prepare", prepare("x")).unwrap();
        let previous = fs::read(f.store.0.path()).unwrap();
        let limit = Store(
            ArchiveStore::with_limits(f.store.0.path(), previous.len() as u64, 1)
                .unwrap()
                .into(),
            Default::default(),
        );
        assert!(
            limit
                .apply(
                    "claim",
                    claim(
                        "x",
                        "long-enough-attempt-to-exceed-the-existing-archive-size"
                    )
                )
                .is_err()
        );
        assert_eq!(fs::read(f.store.0.path()).unwrap(), previous);
        assert_eq!(
            f.store
                .apply("get", json!({"client_request_id":"x"}))
                .unwrap()["state"],
            "prepared"
        );
    }
    #[test]
    fn history_operations_cannot_remove_ledger_permissions_and_recovery_pages_are_complete() {
        let f = Fixture::new();
        for n in 0..105 {
            f.store
                .apply("prepare", prepare(&format!("id-{n:03}")))
                .unwrap();
        }
        let page = f.store.apply("list", json!({})).unwrap();
        assert_eq!(page["intents"].as_array().unwrap().len(), 100);
        let cursor = page["next_cursor"].as_str().unwrap();
        let second = f.store.apply("list", json!({"cursor":cursor})).unwrap();
        assert_eq!(second["intents"].as_array().unwrap().len(), 5);
        assert!(second["next_cursor"].is_null());
        let history = ArchiveStore::new(f.root.join("sms-archive.json"));
        history.replace(vec![]).unwrap();
        assert_eq!(
            f.store.apply("list", json!({})).unwrap()["intents"]
                .as_array()
                .unwrap()
                .len(),
            100
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(f.store.0.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn capacity_and_corruption_reject_without_truncating_tombstones() {
        let f = Fixture::new();
        let mut records = BTreeMap::new();
        for n in 0..MAX_RECORDS {
            let r = Record {
                client_request_id: format!("id-{n}"),
                phone: "p".into(),
                message: "m".into(),
                device: "dji".into(),
                subscription_id: String::new(),
                binding: "b".into(),
                state: "unknown".into(),
                attempt: "a".into(),
                segments: 0,
                archive_confirmed: false,
                created_at_ms: 1,
                updated_at_ms: 1,
            };
            records.insert(r.client_request_id.clone(), r);
        }
        let mut items = vec![];
        write(&mut items, records);
        f.store.0.replace(items).unwrap();
        let before = fs::read(f.store.0.path()).unwrap();
        assert!(f.store.apply("prepare", prepare("extra")).is_err());
        assert_eq!(fs::read(f.store.0.path()).unwrap(), before);
        fs::write(f.store.0.path(), b"malformed").unwrap();
        assert!(f.store.apply("claim", claim("x", "y")).is_err());
        assert_eq!(fs::read(f.store.0.path()).unwrap(), b"malformed");
    }
    async fn http_call(
        f: &Fixture,
        header: &str,
        token: &str,
        action: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        http_call_on_socket(f, header, token, action, body, false).await
    }
    async fn http_call_on_socket(
        f: &Fixture,
        header: &str,
        token: &str,
        action: &str,
        body: Value,
        public_socket: bool,
    ) -> (StatusCode, Value) {
        let sms_inbox = crate::message_archive::InboxState::default();
        let state = crate::ServerState {
            call_mode: Default::default(),
            module_call: Default::default(),
            reader_lifecycle: Default::default(),
            multistandby_card: Default::default(),
            multistandby_service: Default::default(),
            app_token: Some("app-token".into()),
            internal_token: "internal-token".into(),
            multistandby_store: crate::multistandby_store::Store::open(&f.root).unwrap(),
            multistandby_operations: crate::multistandby_operations::Store::open(&f.root).unwrap(),
            device_send_store: f.store.clone(),
            forwarding_handoff_store: crate::forwarding_handoff_store::Store::open(&f.root)
                .unwrap(),
            sms_forwarding: crate::sms_forwarding::Service::default(),
            sms_inbox: sms_inbox.clone(),
            usb_sms_receive: crate::test_usb_sms_receiver(
                sms_inbox.clone(),
                f.root.join("sms-archive.json"),
            ),
            call_archive: proletarius_store::ArchiveStore::new(f.root.join("call-log.json")),
            call_facts: crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(f.root.join("call-log.json")),
                "call-producer-fixture".to_owned(),
            ),
            go_socket_path: std::path::PathBuf::new(),
            availability: crate::BackendAvailability::new(),
            reader_discovery: std::sync::Arc::new(std::sync::Mutex::new(
                crate::ReaderDiscovery::default(),
            )),
            snapshot_instance_id: "fixture".into(),
            snapshot_revision: std::sync::Arc::new(crate::AtomicU64::new(0)),
            eap_exchanges: std::sync::Arc::new(crate::eap_identity::EapExchangeStore::default()),
            reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
            reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
            modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
            qmi_enhancement: crate::qmi_enhancement::QmiEnhancement::default(),
            network_traffic: crate::network_traffic::Service::default(),
            usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
            module_radio: crate::test_module_radio(&f.root),
            native_vowifi_lifecycle: std::sync::Arc::new(
                crate::native_vowifi_session::NativeLifecycle::default(),
            ),
            ims_sms_transactions: crate::ims_sms_transactions::Service::default(),
            multi_runtime_report: std::sync::Arc::new(std::sync::Mutex::new(
                crate::multistandby_runtime_report::Journal::default(),
            )),
        };
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let shutting_down = action.starts_with("shutdown:");
        let action = action.strip_prefix("shutdown:").unwrap_or(action);
        let task = tokio::spawn(async move {
            if shutting_down {
                crate::serve_internal_shutdown_connection(server, state).await;
            } else if public_socket {
                let _ = crate::serve_connection(server, state).await;
            } else {
                let _ = crate::serve_internal_connection(server, state).await;
            }
        });
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(client))
                .await
                .unwrap();
        let connection = tokio::spawn(connection);
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("{PREFIX}{action}"))
            .header(header, token)
            .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
            .unwrap();
        let response = sender.send_request(req).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap();
        drop(sender);
        connection.abort();
        task.abort();
        (status, value)
    }
    #[tokio::test]
    async fn actual_gateway_requires_private_token_and_never_exposes_native_claim_to_app_auth() {
        let f = Fixture::new();
        for (header, token) in [
            ("x-proletarius-app-token", "app-token"),
            ("x-switch-internal-token", "internal-token"),
        ] {
            let (status, body) = http_call_on_socket(
                &f,
                header,
                token,
                "prepare",
                prepare("public-must-not-prepare"),
                true,
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(body["code"], "route_not_found");
        }
        assert!(
            f.store
                .apply(
                    "get",
                    json!({"client_request_id":"public-must-not-prepare"})
                )
                .is_err()
        );
        for (header, token) in [
            ("x-proletarius-app-token", "app-token"),
            ("x-switch-internal-token", "wrong"),
        ] {
            assert_eq!(
                http_call(&f, header, token, "prepare", prepare("http"))
                    .await
                    .0,
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            http_call(
                &f,
                "x-switch-internal-token",
                "internal-token",
                "prepare",
                prepare("http")
            )
            .await
            .0,
            StatusCode::OK
        );
        let (first_status, first) = http_call(
            &f,
            "x-switch-internal-token",
            "internal-token",
            "claim",
            claim("http", "attempt-A"),
        )
        .await;
        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(first["can_submit"], true);
        let (_, again) = http_call(
            &f,
            "x-switch-internal-token",
            "internal-token",
            "claim",
            claim("http", "attempt-B"),
        )
        .await;
        assert_eq!(again["can_submit"], false);
    }
    #[tokio::test]
    async fn shutdown_allows_only_authenticated_final_outcome_without_new_claim() {
        let f = Fixture::new();
        f.store.apply("prepare", prepare("shutdown")).unwrap();
        f.store
            .apply("claim", claim("shutdown", "same-attempt"))
            .unwrap();
        let finish = json!({"client_request_id":"shutdown","attempt":"same-attempt","state":"accepted","segments":1,"archive_confirmed":true});
        assert_eq!(
            http_call(
                &f,
                "x-proletarius-app-token",
                "app-token",
                "shutdown:finish",
                finish.clone()
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            http_call(
                &f,
                "x-switch-internal-token",
                "internal-token",
                "shutdown:claim",
                claim("shutdown", "new-attempt")
            )
            .await
            .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let (status, result) = http_call(
            &f,
            "x-switch-internal-token",
            "internal-token",
            "shutdown:finish",
            finish,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["state"], "accepted");
    }
    #[test]
    fn semantic_corruption_cannot_turn_previous_attempt_into_prepared_permission() {
        let f = Fixture::new();
        f.store.apply("prepare", prepare("corrupt")).unwrap();
        let bytes = fs::read(f.store.0.path()).unwrap();
        let base: Value = serde_json::from_slice(&bytes).unwrap();
        for (field, value) in [
            ("attempt", json!("already-submitted")),
            ("segments", json!(1)),
            ("archive_confirmed", json!(true)),
            ("device", json!("unknown")),
            ("message", json!("")),
            ("binding", json!("")),
        ] {
            let mut v = base.clone();
            v[0]["records"]["corrupt"][field] = value;
            let corrupted = serde_json::to_vec(&v).unwrap();
            fs::write(f.store.0.path(), &corrupted).unwrap();
            assert!(
                f.store
                    .apply("claim", claim("corrupt", "new-attempt"))
                    .is_err(),
                "{field}"
            );
            assert_eq!(fs::read(f.store.0.path()).unwrap(), corrupted);
        }
        let mut terminal = base;
        terminal[0]["records"]["corrupt"]["state"] = json!("accepted");
        let bytes = serde_json::to_vec(&terminal).unwrap();
        fs::write(f.store.0.path(), &bytes).unwrap();
        assert!(f.store.recover().is_err());
        assert_eq!(fs::read(f.store.0.path()).unwrap(), bytes);
    }
    #[test]
    fn existing_empty_or_cleared_ledger_is_not_a_fresh_authorization_store() {
        let f = Fixture::new();
        for bytes in [b"".as_slice(), b"null".as_slice(), b"[]".as_slice()] {
            fs::write(f.store.0.path(), bytes).unwrap();
            assert!(f.store.apply("prepare", prepare("new")).is_err());
            assert_eq!(fs::read(f.store.0.path()).unwrap(), bytes);
        }
    }
    fn v2_reservation(store: &Store) -> Value {
        let status=store.apply("authority",json!({})).unwrap();
        json!({"identity_version":2,"family":"device_sms","authority_id":status["authority_id"],
            "epoch":status["epoch"],"sequence":status["next_sequence"],
            "client_request_id":status["next_client_request_id"],
            "intent_nonce":"0123456789abcdef0123456789abcdef","purpose":"device_sms_send","device":"reader",
            "payload":{"phone":"+12025550100","message":"fixture","expected_subscription_id":""}})
    }
    fn finish_v2(store: &Store) -> Value {
        let reserved=store.apply("reserve",v2_reservation(store)).unwrap();
        store.apply("prepare",json!({"identity_version":2,"intent_nonce":reserved["intent_nonce"],
            "client_request_id":reserved["client_request_id"],"phone":"+12025550100","message":"fixture",
            "device":"reader","expected_subscription_id":"","subscription_id":"","binding":"v2-route"})).unwrap();
        store.apply("claim",json!({"identity_version":2,"intent_nonce":reserved["intent_nonce"],
            "client_request_id":reserved["client_request_id"],"binding":"v2-route","attempt":"v2-attempt"})).unwrap();
        store.apply("finish",json!({"client_request_id":reserved["client_request_id"],
            "attempt":"v2-attempt","state":"accepted","segments":1,"archive_confirmed":true})).unwrap()
    }
    fn archive_row(record: &Value) -> Value {
        let timestamp=time::OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(record["created_at_ms"].as_u64().unwrap())*1_000_000).unwrap()
            .format(&time::format_description::well_known::Rfc3339).unwrap();
        json!({"send_request_id":record["client_request_id"],"sender":record["phone"],
            "content":record["message"],"device":record["device"],"subscription_id":record["subscription_id"],
            "direction":"out","timestamp":timestamp})
    }

    #[test]
    fn fresh_root_initializes_v2_but_missing_active_never_recreates_authority() {
        let f=Fixture::new();
        {
            let lease=crate::daemon_lifetime::Lease::acquire(&f.root).unwrap();
            let store=Store::open_with_authority(&f.root,lease.root_proof()).unwrap();
            assert_eq!(store.apply("authority",json!({})).unwrap()["issued_high_water"],"0");
            assert!(f.root.join("device-send-ledger.legacy-v1.json").is_file());
            let legacy:Vec<Value>=serde_json::from_slice(&fs::read(f.root.join("device-send-ledger.legacy-v1.json")).unwrap()).unwrap();
            assert_eq!(read(&legacy,true).unwrap().len(),0);
            assert!(read(&serde_json::from_slice::<Vec<Value>>(&fs::read(f.root.join("device-send-ledger.json")).unwrap()).unwrap(),true).is_err());
        }
        fs::remove_file(f.root.join("device-send-ledger.json")).unwrap();
        let lease=crate::daemon_lifetime::Lease::acquire(&f.root).unwrap();
        assert!(Store::open_with_authority(&f.root,lease.root_proof()).is_err());
        assert!(!f.root.join("device-send-ledger.json").exists());
    }

    #[tokio::test]
    async fn v2_retired_imported_and_legacy_archives_remain_deletable() {
        let f=Fixture::new();
        f.store.apply("prepare",prepare("legacy-archive")).unwrap();
        f.store.apply("claim",claim("legacy-archive","legacy-attempt")).unwrap();
        let legacy=f.store.apply("finish",json!({"client_request_id":"legacy-archive",
            "attempt":"legacy-attempt","state":"accepted","segments":1,"archive_confirmed":false})).unwrap();
        let lease=crate::daemon_lifetime::Lease::acquire(&f.root).unwrap();
        let store=Store::open_with_authority(&f.root,lease.root_proof()).unwrap();
        let first=finish_v2(&store);
        let archive=ArchiveStore::new(f.root.join("sms-archive.json"));
        let legacy_item=archive_row(&legacy);
        let first_item=archive_row(&first);
        let mut imported=first_item.clone();imported["send_request_id"]=json!("imported-without-ledger");
        crate::message_archive::ingest_messages_to(archive.clone(),
            vec![legacy_item.clone(),first_item.clone(),imported.clone()]).await.unwrap();
        for _ in 0..40 {finish_v2(&store);}
        assert!(matches!(store.apply("get",json!({"client_request_id":first["client_request_id"]})),
            Err(StoreError::Configuration(code)) if code=="mutation_identity_retired"));
        for item in [legacy_item,first_item,imported] {
            let id=crate::message_archive::cache_digest(&item);
            assert_eq!(crate::message_archive::delete_ids_from_store(archive.clone(),vec![id]).await.unwrap(),1);
        }
        assert!(archive.load().unwrap().is_empty());
        assert_eq!(store.apply("get",json!({"client_request_id":"legacy-archive"})).unwrap()["archive_confirmed"],true);
        assert!(!store.status().restart_required);
    }

    #[test]
    fn full_v1_migrates_without_removing_any_identity_and_old_ids_cannot_claim() {
        let f=Fixture::new();
        let mut records=BTreeMap::new();
        for n in 0..MAX_RECORDS {
            let id=format!("full-legacy-{n}");
            records.insert(id.clone(),Record{client_request_id:id,phone:"+12025550100".into(),
                message:"fixture".into(),device:"reader".into(),subscription_id:String::new(),
                binding:"old-route".into(),state:"accepted".into(),attempt:"old-attempt".into(),
                segments:1,archive_confirmed:true,created_at_ms:1000,updated_at_ms:1000});
        }
        f.store.0.mutate(|items|{write(items,records);Ok(())}).unwrap();
        let original=fs::read(f.root.join("device-send-ledger.json")).unwrap();
        let lease=crate::daemon_lifetime::Lease::acquire(&f.root).unwrap();
        let store=Store::open_with_authority(&f.root,lease.root_proof()).unwrap();
        assert_eq!(fs::read(f.root.join("device-send-ledger.legacy-v1.json")).unwrap(),original);
        assert_eq!(store.apply("get",json!({"client_request_id":"full-legacy-9999"})).unwrap()["attempt"],"old-attempt");
        assert!(store.apply("claim",json!({"client_request_id":"full-legacy-9999",
            "binding":"old-route","attempt":"new-attempt"})).is_err());
        assert_eq!(finish_v2(&store)["archive_confirmed"],true);
    }

}
