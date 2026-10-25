//! One-time durable import of the two Swift forwarding admission cursors.
//!
//! This ledger is deliberately separate from the provider queue. It records
//! the exact Swift snapshot and gives retries a durable idempotency boundary;
//! it does not start a provider, advance a cursor, or authorize cutting over
//! either Swift producer.

use std::path::Path;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode};
use proletarius_api_contract::SmsForwardingHandoffV1;
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::json;

const STORE_FILE: &str = "sms-forwarding-handoff.json";
const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const BODY_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const ADMISSION_BUSY: &str = "handoff_admission_busy";
static HANDOFF_ADMISSION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

// Serialize test fixture handoff initialization only. Production overload still
// uses HANDOFF_ADMISSION.try_acquire(); no request is silently retried here.
#[cfg(test)]
pub(crate) static REQUEST_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
pub(crate) struct Store(crate::sms_store_health::GuardedArchive);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LedgerRecord {
    schema_version: u16,
    handoff: SmsForwardingHandoffV1,
}

#[derive(Serialize)]
struct HandoffAck<'a> {
    accepted: bool,
    handoff_id: &'a str,
    already_accepted: bool,
}

#[derive(Serialize)]
struct HandoffSnapshot<'a> {
    accepted: bool,
    snapshot: Option<&'a SmsForwardingHandoffV1>,
}

impl Store {
    pub(crate) fn open(root: &Path) -> Result<Self, StoreError> {
        Ok(Self(
            ArchiveStore::with_limits(root.join(STORE_FILE), MAX_ARCHIVE_BYTES, 1)?.into(),
        ))
    }

    fn accept(&self, handoff: SmsForwardingHandoffV1) -> Result<bool, StoreError> {
        handoff
            .validate()
            .map_err(|_| StoreError::Configuration("invalid_request".to_owned()))?;
        let handoff_id = handoff.handoff_id.clone();
        self.0.mutate(|items| {
            if items.len() > 1 {
                return Err(corrupt());
            }
            let record = if let Some(value) = items.first() {
                let record: LedgerRecord =
                    serde_json::from_value(value.clone()).map_err(|_| corrupt())?;
                if record.schema_version != 1 || record.handoff.validate().is_err() {
                    return Err(corrupt());
                }
                if record.handoff.handoff_id != handoff_id || record.handoff != handoff {
                    return Err(conflict());
                }
                record
            } else {
                LedgerRecord {
                    schema_version: 1,
                    handoff,
                }
            };
            let already_accepted = !items.is_empty();
            let value = serde_json::to_value(record).map_err(StoreError::Json)?;
            *items = vec![value];
            Ok(already_accepted)
        })
    }

    pub(crate) fn current(&self) -> Result<Option<SmsForwardingHandoffV1>, StoreError> {
        self.0.read(|items| {
            if items.len() > 1 {
                return Err(corrupt());
            }
            let Some(value) = items.first() else {
                return Ok(None);
            };
            let record: LedgerRecord =
                serde_json::from_value(value.clone()).map_err(|_| corrupt())?;
            if record.schema_version != 1 || record.handoff.validate().is_err() {
                return Err(corrupt());
            }
            Ok(Some(record.handoff))
        })
    }
}

pub(crate) async fn handle<B>(request: Request<B>, store: Store) -> Response<Full<Bytes>>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if request.method() != Method::GET && request.method() != Method::POST {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed", false);
    }
    let _permit = match HANDOFF_ADMISSION.try_acquire() {
        Ok(permit) => permit,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, ADMISSION_BUSY, true),
    };
    if request.method() == Method::GET {
        return match crate::archive_io::run(move || store.current()).await {
            Ok(snapshot) => crate::json_response(
                StatusCode::OK,
                &HandoffSnapshot {
                    accepted: snapshot.is_some(),
                    snapshot: snapshot.as_ref(),
                },
            ),
            Err(_) => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "handoff_store_unavailable",
                false,
            ),
        };
    }
    let body = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        MAX_REQUEST_BYTES,
        BODY_READ_TIMEOUT,
    )
    .await
    {
        Err(crate::http_server::BodyReadError::Timeout) => {
            return error(StatusCode::REQUEST_TIMEOUT, "request_body_timeout", true);
        }
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_body_too_large",
                false,
            );
        }
        Ok(body) => body,
    };
    let handoff: SmsForwardingHandoffV1 = match serde_json::from_slice(&body) {
        Ok(handoff) => handoff,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_request", false),
    };
    if handoff.validate().is_err() {
        return error(StatusCode::BAD_REQUEST, "invalid_request", false);
    }
    let handoff_id = handoff.handoff_id.clone();
    let stored = crate::archive_io::run(move || store.accept(handoff)).await;
    match stored {
        Ok(already_accepted) => crate::json_response(
            StatusCode::OK,
            &HandoffAck {
                accepted: true,
                handoff_id: &handoff_id,
                already_accepted,
            },
        ),
        Err(StoreError::Configuration(code)) if code == "state_conflict" => {
            error(StatusCode::CONFLICT, "handoff_conflict", false)
        }
        Err(StoreError::Configuration(code)) if code == "invalid_request" => {
            error(StatusCode::BAD_REQUEST, "invalid_request", false)
        }
        Err(StoreError::TooLarge { .. } | StoreError::TooManyItems { .. }) => {
            error(StatusCode::PAYLOAD_TOO_LARGE, "handoff_store_limit", false)
        }
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "handoff_store_unavailable",
            false,
        ),
    }
}

fn corrupt() -> StoreError {
    StoreError::Configuration("ledger_corrupt".to_owned())
}

fn conflict() -> StoreError {
    StoreError::Configuration("state_conflict".to_owned())
}

fn error(status: StatusCode, code: &'static str, retryable: bool) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"code":code,"error":code,"retryable":retryable}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use hyper::Request;
    use serde_json::{Value, json};
    use std::fs;

    struct TestRoot(std::path::PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let switch_root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .expect("Switch root from Cargo manifest");
            let parent = switch_root.join("build/tmp/forwarding-handoff-tests");
            fs::create_dir_all(&parent).unwrap();
            let root = parent.join(crate::multistandby_store::random_token().unwrap());
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn handoff(id: &str) -> SmsForwardingHandoffV1 {
        serde_json::from_value(json!({
            "schema_version": 1,
            "handoff_id": id,
            "device_sms": {
                "accepted_notification_ids": ["a".repeat(64)],
                "was_enabled": true
            },
            "nwait_sms": {
                "cursor": "opaque-cursor",
                "seeded": true,
                "was_enabled": false
            }
        }))
        .unwrap()
    }

    const FIRST_ID: &str = "123e4567-e89b-12d3-a456-426614174000";

    #[test]
    fn durable_ack_survives_reopen_and_exact_replay_is_idempotent() {
        let root = TestRoot::new();
        let first = Store::open(&root.0).unwrap();
        assert!(!first.accept(handoff(FIRST_ID)).unwrap());

        let reopened = Store::open(&root.0).unwrap();
        assert!(reopened.accept(handoff(FIRST_ID)).unwrap());
        let bytes = fs::read(root.0.join(STORE_FILE)).unwrap();
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .contains("opaque-cursor")
        );
        assert!(
            !std::str::from_utf8(&bytes)
                .unwrap()
                .contains("sms_body_must_never_be_in_handoff")
        );
    }

    #[test]
    fn same_handoff_id_with_changed_snapshot_conflicts_without_overwriting_durable_state() {
        let root = TestRoot::new();
        let store = Store::open(&root.0).unwrap();
        store.accept(handoff(FIRST_ID)).unwrap();
        let before = fs::read(root.0.join(STORE_FILE)).unwrap();

        let mut changed = handoff(FIRST_ID);
        changed.nwait_sms.cursor = Some("different-cursor".to_owned());
        assert!(
            matches!(store.accept(changed), Err(StoreError::Configuration(code)) if code == "state_conflict")
        );
        assert_eq!(fs::read(root.0.join(STORE_FILE)).unwrap(), before);
    }

    #[test]
    fn another_handoff_id_cannot_replace_the_first_snapshot() {
        let root = TestRoot::new();
        let store = Store::open(&root.0).unwrap();
        store.accept(handoff(FIRST_ID)).unwrap();
        let mut second = handoff("123e4567-e89b-12d3-a456-426614174001");
        second.device_sms.was_enabled = Some(false);
        assert!(
            matches!(store.accept(second), Err(StoreError::Configuration(code)) if code == "state_conflict")
        );
    }

    #[tokio::test]
    async fn endpoint_returns_durable_idempotency_ack_and_rejects_malformed_payload() {
        let _guard = REQUEST_TEST_LOCK.lock().await;
        let root = TestRoot::new();
        let store = Store::open(&root.0).unwrap();
        let empty = Request::get("/api/v2/sms/forwarding/handoff")
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();
        let empty = handle(empty, store.clone()).await;
        assert_eq!(empty.status(), StatusCode::OK);
        let empty: Value =
            serde_json::from_slice(&empty.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(empty["accepted"], false);
        assert!(empty["snapshot"].is_null());

        let body = serde_json::to_vec(&handoff(FIRST_ID)).unwrap();
        let request = Request::post("/api/v2/sms/forwarding/handoff")
            .body(http_body_util::Full::new(Bytes::from(body)))
            .unwrap();
        let response = handle(request, store.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let ack: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(ack["accepted"], true);
        assert_eq!(ack["already_accepted"], false);

        let current = Request::get("/api/v2/sms/forwarding/handoff")
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();
        let current = handle(current, store.clone()).await;
        assert_eq!(current.status(), StatusCode::OK);
        let current: Value =
            serde_json::from_slice(&current.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(current["accepted"], true);
        assert_eq!(current["snapshot"]["handoff_id"], FIRST_ID);
        assert_eq!(current["snapshot"]["nwait_sms"]["cursor"], "opaque-cursor");

        let replay = Request::post("/api/v2/sms/forwarding/handoff")
            .body(http_body_util::Full::new(Bytes::from(
                serde_json::to_vec(&handoff(FIRST_ID)).unwrap(),
            )))
            .unwrap();
        let replay = handle(replay, store).await;
        let ack: Value =
            serde_json::from_slice(&replay.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(ack["already_accepted"], true);

        let invalid = Request::post("/api/v2/sms/forwarding/handoff")
            .body(http_body_util::Full::new(Bytes::from_static(b"{}")))
            .unwrap();
        assert_eq!(
            handle(invalid, Store::open(&root.0).unwrap())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn oversized_request_is_rejected_before_creating_the_handoff_file() {
        let _guard = REQUEST_TEST_LOCK.lock().await;
        let root = TestRoot::new();
        let store = Store::open(&root.0).unwrap();
        let request = Request::post("/api/v2/sms/forwarding/handoff")
            .body(http_body_util::Full::new(Bytes::from(vec![
                b'x';
                MAX_REQUEST_BYTES
                    + 1
            ])))
            .unwrap();
        let response = handle(request, store).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(!root.0.join(STORE_FILE).exists());
    }

    #[test]
    fn malformed_existing_handoff_is_never_replaced_by_a_new_snapshot() {
        use std::os::unix::fs::PermissionsExt;

        let root = TestRoot::new();
        let path = root.0.join(STORE_FILE);
        fs::write(&path, b"not-json").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::read(&path).unwrap();
        let store = Store::open(&root.0).unwrap();
        assert!(store.accept(handoff(FIRST_ID)).is_err());
        assert_eq!(store.0.status().state, "unavailable");
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[tokio::test]
    async fn busy_handoff_refuses_before_store_io_then_same_request_commits_after_release() {
        let _fixture = REQUEST_TEST_LOCK.lock().await;
        let root = TestRoot::new();
        let store = Store::open(&root.0).unwrap();
        let owner = HANDOFF_ADMISSION.try_acquire().unwrap();
        let request = || {
            Request::post("/api/v2/sms/forwarding/handoff")
                .body(http_body_util::Full::new(Bytes::from(
                    serde_json::to_vec(&handoff(FIRST_ID)).unwrap(),
                )))
                .unwrap()
        };
        let response = handle(request(), store.clone()).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["code"], "handoff_admission_busy");
        assert_eq!(value["retryable"], true);
        assert!(!root.0.join(STORE_FILE).exists());
        assert!(!root.0.join(format!("{STORE_FILE}.lock")).exists());
        drop(owner);
        let response = handle(request(), store.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["accepted"], true);
        assert_eq!(value["already_accepted"], false);
        assert!(store.current().unwrap() == Some(handoff(FIRST_ID)));
    }
}
