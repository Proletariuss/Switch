//! A store failure disables only that store for this process. No failed read or
//! write is treated as an empty archive, and no live request clears the latch.
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};

use proletarius_api_contract::{SmsStoreHealth, SmsStoreStatus};
use proletarius_store::raw::{Mutation, TransactionError};
use proletarius_store::{ArchiveStore, StoreError};
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct GuardedArchive {
    archive: ArchiveStore,
    serial: Arc<Mutex<()>>,
    fault: Arc<AtomicU8>,
    observed_present: Arc<AtomicBool>,
}

impl From<ArchiveStore> for GuardedArchive {
    fn from(archive: ArchiveStore) -> Self {
        let archive = archive.track_durable_presence();
        Self {
            observed_present: Arc::new(AtomicBool::new(archive.path().exists())),
            archive,
            serial: Arc::new(Mutex::new(())),
            fault: Arc::new(AtomicU8::new(0)),
        }
    }
}

impl GuardedArchive {
    pub(crate) fn path(&self) -> &Path {
        self.archive.path()
    }

    pub(crate) fn status(&self) -> SmsStoreStatus {
        if self.serial.is_poisoned() {
            self.latch(5);
        }
        let reason = match self.fault.load(Ordering::Acquire) {
            0 => None,
            1 => Some("store_corrupt"),
            2 => Some("store_io_failed"),
            3 => Some("store_path_unsafe"),
            4 => Some("store_limit_exceeded"),
            _ => Some("store_worker_failed"),
        };
        SmsStoreStatus {
            state: if reason.is_none() {
                "ready"
            } else {
                "unavailable"
            },
            reason,
            restart_required: reason.is_some(),
        }
    }

    fn latch(&self, reason: u8) {
        let _ = self
            .fault
            .compare_exchange(0, reason, Ordering::AcqRel, Ordering::Acquire);
    }

    fn checked<T>(
        &self,
        operation: impl FnOnce() -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        // Serialize disk admission and fault publication. A queued claim cannot
        // pass after another operation returned an uncertain persistence result.
        let _guard = self.serial.lock().map_err(|_| {
            self.latch(5);
            unavailable()
        })?;
        if self.fault.load(Ordering::Acquire) != 0 {
            return Err(unavailable());
        }
        let result = operation();
        if let Err(error) = &result {
            if let Some(reason) = fault_kind(error) {
                self.latch(reason);
                return Err(unavailable());
            }
        }
        if result.is_ok() && self.path().exists() {
            self.observed_present.store(true, Ordering::Release);
        }
        result
    }

    /// Existing-authority raw transactions share the same admission lock and
    /// sticky failure latch as legacy reads/writes. Pure callbacks run under
    /// both locks; dispatch must happen only after this method returns Ok.
    /// Preserve the original error (including Before/AfterRename) for recovery
    /// diagnostics. A later request sees unavailable and never runs a callback.
    pub(crate) fn transaction_raw<T>(
        &self,
        validate: impl Fn(&[u8]) -> Result<(), StoreError>,
        plan: impl FnOnce(&[u8]) -> Result<Mutation<T>, StoreError>,
    ) -> Result<T, TransactionError<StoreError, StoreError>> {
        self.checked_raw(|| self.archive.transaction_raw(validate, plan))
    }

    fn checked_raw<T>(
        &self,
        operation: impl FnOnce() -> Result<T, TransactionError<StoreError, StoreError>>,
    ) -> Result<T, TransactionError<StoreError, StoreError>> {
        use TransactionError::*;
        let _guard = self.serial.lock().map_err(|_| {
            self.latch(5);
            Current(unavailable())
        })?;
        if self.fault.load(Ordering::Acquire) != 0 {
            return Err(Current(unavailable()));
        }
        let result = operation();
        if let Err(error) = &result {
            let reason = match error {
                // Configuration errors from the raw kernel are integrity
                // failures even when the legacy business-error table does not
                // know their code (named lock, presence or authority changed).
                Current(source) => Some(fault_kind(source).unwrap_or(1)),
                CurrentSchema(_) => Some(1),
                // Invalid output is an adapter/worker bug, never a safe grant.
                Proposal(_) | ProposalSchema(_) => Some(5),
                Commit(error) => Some(fault_kind(&error.source).unwrap_or(2)),
                // This pilot's only reviewed pure business refusal. Unknown
                // codes, including schema errors disguised as rejection, close
                // the latch until a future caller adds its reviewed contract.
                Rejected(StoreError::Configuration(code)) if code == "admission_state_conflict" => {
                    None
                }
                Rejected(source) => Some(fault_kind(source).unwrap_or(5)),
            };
            if let Some(reason) = reason {
                self.latch(reason);
            }
        }
        if result.is_ok() {
            self.observed_present.store(true, Ordering::Release);
        }
        result
    }

    fn nonempty_if_present(&self, items: &[Value]) -> Result<(), StoreError> {
        if items.is_empty()
            && (self.observed_present.load(Ordering::Acquire)
                || self.path().try_exists().map_err(StoreError::Io)?)
        {
            return Err(StoreError::Configuration("invalid_store_schema".into()));
        }
        Ok(())
    }

    pub(crate) fn read<T>(
        &self,
        decode: impl FnOnce(&[Value]) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.checked(|| {
            let items = self.archive.load()?;
            self.nonempty_if_present(&items)?;
            decode(&items)
        })
    }

    // Pin only the file descriptor. Export validates its bounded stream outside
    // the admission lock and reports source errors through snapshot_failed.
    pub(crate) fn snapshot_file(&self) -> Result<Option<std::fs::File>, StoreError> {
        self.checked(|| {
            let file = self.archive.snapshot_file()?;
            let invalid = match &file {
                Some(file) => file.metadata().map_err(StoreError::Io)?.len() == 0,
                None => self.observed_present.load(Ordering::Acquire),
            };
            if invalid {
                return Err(StoreError::Configuration("invalid_store_schema".into()));
            }
            Ok(file)
        })
    }

    pub(crate) fn snapshot_failed(&self, error: StoreError) -> StoreError {
        self.checked::<()>(|| Err(error))
            .expect_err("source failure")
    }

    #[cfg(test)]
    pub(crate) fn load(&self) -> Result<Vec<Value>, StoreError> {
        self.read(|items| Ok(items.to_vec()))
    }

    #[cfg(test)]
    pub(crate) fn replace(&self, items: Vec<Value>) -> Result<(), StoreError> {
        self.checked(|| self.archive.replace(items))
    }

    pub(crate) fn mutate<T>(
        &self,
        mutation: impl FnOnce(&mut Vec<Value>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.checked(|| {
            self.archive.mutate(|items| {
                self.nonempty_if_present(items)?;
                mutation(items)
            })
        })
    }

    pub(crate) fn mutate_if_changed<T>(
        &self,
        mutation: impl FnOnce(&mut Vec<Value>) -> Result<(T, bool), StoreError>,
    ) -> Result<T, StoreError> {
        self.checked(|| {
            self.archive.mutate_if_changed(|items| {
                self.nonempty_if_present(items)?;
                mutation(items)
            })
        })
    }
}

fn fault_kind(error: &StoreError) -> Option<u8> {
    match error {
        StoreError::Configuration(code) => match code.as_str() {
            "ledger_corrupt"
            | "device_forwarding_ledger_corrupt"
            | "invalid_store_schema"
            | "invalid_durable_presence_store" => Some(1),
            // Invalid user input, stale revisions, missing records and capacity
            // refusals do not mean previously persisted evidence is untrusted.
            _ => None,
        },
        StoreError::Json(_) | StoreError::RootIsNotArray | StoreError::ItemIsNotObject => Some(1),
        StoreError::Io(_) => Some(2),
        StoreError::SymlinkRejected
        | StoreError::NotRegularFile
        | StoreError::MissingParent
        | StoreError::MissingFileName => Some(3),
        StoreError::TooLarge { .. }
        | StoreError::TooManyItems { .. }
        | StoreError::InvalidLimit => Some(4),
    }
}

pub(crate) fn unavailable() -> StoreError {
    StoreError::Configuration("sms_store_unavailable".into())
}

pub(crate) fn snapshot(device_send: SmsStoreStatus, nwait: SmsStoreStatus) -> SmsStoreHealth {
    SmsStoreHealth {
        schema_version: 1,
        degraded: device_send.restart_required || nwait.restart_required,
        device_send,
        nwait,
    }
}

pub(crate) fn rejection(
    status: SmsStoreStatus,
) -> Option<hyper::Response<http_body_util::Full<bytes::Bytes>>> {
    status.restart_required.then(|| crate::json_response(hyper::StatusCode::SERVICE_UNAVAILABLE,
        &serde_json::json!({"code":"sms_store_unavailable","error":"短信持久记录不可用；未授权新提交，请保留原记录并检查诊断","retryable":false,"reason":status.reason,"restart_required":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::{BodyExt, Full};
    use hyper::{Method, Request, StatusCode};
    use serde_json::json;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn native_incoming_event(
        generation: crate::native_vowifi_supervisor::SessionGeneration,
    ) -> crate::native_ims_actor::Event {
        use proletarius_vowifi::sip_session_runtime::{RegisteredSipEvent, RegisteredSipMessage};
        let tpdu = [
            0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x41,
        ];
        let mut body = vec![1, 42, 0, 0, tpdu.len() as u8];
        body.extend_from_slice(&tpdu);
        let mut wire = format!("MESSAGE sip:reader@ims.example SIP/2.0\r\nVia: SIP/2.0/TCP pcscf;branch=z9hG4bKnative\r\nFrom: <sip:smsc@ims.example>\r\nP-Asserted-Identity: <sip:smsc@ims.example>\r\nTo: <sip:reader@ims.example>\r\nCall-ID: native-delivery@example\r\nCSeq: 73 MESSAGE\r\nContent-Type: application/vnd.3gpp.sms\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        wire.extend_from_slice(&body);
        let (proletarius_sip::Message::Request(request), _) = proletarius_sip::Parser::default()
            .parse(&wire, false)
            .unwrap()
        else {
            panic!("fixture request")
        };
        crate::native_ims_actor::Event {
            reply_state: crate::native_ims_actor::IncomingReply::Unanswered,
            generation,
            event: RegisteredSipEvent::Message {
                connection: proletarius_vowifi::registration_coordinator::SipConnectionId::Primary,
                message: Box::new(RegisteredSipMessage::Request {
                    method: request.method,
                    uri: request.uri,
                    version: request.data.version,
                    headers: request.data.headers,
                    body: zeroize::Zeroizing::new(request.data.body),
                }),
                response_ticket: None,
            },
        }
    }

    struct Fixture {
        root: std::path::PathBuf,
        state: crate::ServerState,
    }
    impl Fixture {
        fn new(damaged: Option<(&str, &[u8])>) -> Self {
            let root = std::env::temp_dir().join(format!(
                "sms-store-health-{}",
                crate::multistandby_store::random_token().unwrap()
            ));
            fs::create_dir_all(&root).unwrap();
            if let Some((path, bytes)) = damaged {
                let path = root.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, bytes).unwrap();
            }
            let (nwait, device, forwarding_handoff) = crate::initialize_sms_stores(&root).unwrap();
            let operations = crate::multistandby_operations::Store::open(&root).unwrap();
            let sms_inbox = crate::message_archive::InboxState::default();
            let state = crate::ServerState {
                call_mode: Default::default(),
                module_call: Default::default(),
                reader_lifecycle: Default::default(),
                multistandby_card: Default::default(),
                multistandby_service: Default::default(),
                app_token: Some("fixture-app".into()),
                internal_token: "fixture-private".into(),
                multistandby_store: nwait,
                multistandby_operations: operations,
                device_send_store: device,
                forwarding_handoff_store: forwarding_handoff,
                sms_forwarding: crate::sms_forwarding::Service::default(),
                sms_inbox: sms_inbox.clone(),
                usb_sms_receive: crate::test_usb_sms_receiver(
                    sms_inbox.clone(),
                    root.join("sms-archive.json"),
                ),
                call_archive: proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                call_facts: crate::call_facts::CallFacts::new(
                    proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                    "call-producer-fixture".to_owned(),
                ),
                go_socket_path: root.join("not-running.sock"),
                availability: crate::BackendAvailability::new(),
                reader_discovery: Arc::new(Mutex::new(crate::ReaderDiscovery::default())),
                snapshot_instance_id: "fixture".into(),
                snapshot_revision: Arc::new(crate::AtomicU64::new(0)),
                eap_exchanges: Arc::new(crate::eap_identity::EapExchangeStore::default()),
                reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
                reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
                modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
                qmi_enhancement: crate::qmi_enhancement::QmiEnhancement::default(),
                network_traffic: crate::network_traffic::Service::default(),
                usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
                module_radio: crate::test_module_radio(&root),
                native_vowifi_lifecycle: std::sync::Arc::new(
                    crate::native_vowifi_session::NativeLifecycle::default(),
                ),
                ims_sms_transactions: crate::ims_sms_transactions::Service::default(),
                multi_runtime_report: Arc::new(Mutex::new(
                    crate::multistandby_runtime_report::Journal::default(),
                )),
            };
            Self { root, state }
        }
        async fn call(
            &self,
            method: Method,
            path: &str,
            private: bool,
            body: Value,
        ) -> (StatusCode, Value) {
            let (client, server) = tokio::net::UnixStream::pair().unwrap();
            let state = self.state.clone();
            let serving = tokio::spawn(async move {
                if private {
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
            let (header, token) = if private {
                ("x-switch-internal-token", "fixture-private")
            } else {
                ("x-proletarius-app-token", "fixture-app")
            };
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header(header, token)
                .body(Full::new(bytes::Bytes::from(
                    serde_json::to_vec(&body).unwrap(),
                )))
                .unwrap();
            let response = sender.send_request(request).await.unwrap();
            let status = response.status();
            let body = response.into_body().collect().await.unwrap().to_bytes();
            drop(sender);
            connection.abort();
            serving.abort();
            (status, serde_json::from_slice(&body).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn corrupt_existing_stores_never_initialize_empty_and_only_failed_source_degrades() {
        for path in ["device-send-ledger.json", "multistandby/store-v1.json"] {
            for bytes in [
                b"{broken".as_slice(),
                b"",
                b"null",
                b"[]",
                b"[{\"schema_version\":999}]",
            ] {
                let fixture = Fixture::new(Some((path, bytes)));
                let device = fixture.state.device_send_store.status();
                let nwait = fixture.state.multistandby_store.status();
                assert_eq!(device.restart_required, path.starts_with("device"));
                assert_eq!(nwait.restart_required, path.starts_with("multistandby"));
                let failed = if device.restart_required {
                    device
                } else {
                    nwait
                };
                assert_eq!(failed.reason, Some("store_corrupt"));
                assert_eq!(fs::read(fixture.root.join(path)).unwrap(), bytes);
            }
        }
    }

    #[tokio::test]
    async fn runtime_truth_and_all_device_send_entrypoints_remain_fail_closed() {
        let f = Fixture::new(Some(("device-send-ledger.json", b"invalid")));
        let (status, value) = f
            .call(Method::GET, "/api/v2/runtime", false, Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["ok"], true);
        assert_eq!(
            value["sms_store_health"],
            json!({"schema_version":1,"degraded":true,
            "device_send":{"state":"unavailable","reason":"store_corrupt","restart_required":true},
            "nwait":{"state":"ready","reason":null,"restart_required":false}})
        );
        for (method, path, private) in [
            (Method::POST, "/api/sms/send", false),
            (Method::POST, "/api/sms/send-intents", false),
            (
                Method::GET,
                "/api/sms/send-intents?client_request_id=fixture",
                false,
            ),
            (Method::POST, "/api/internal/device-send/prepare", true),
            (Method::POST, "/api/internal/device-send/claim", true),
            (Method::POST, "/api/internal/device-send/finish", true),
            (Method::POST, "/api/internal/device-send/get", true),
            (Method::POST, "/api/internal/device-send/list", true),
        ] {
            let (status, value) = f
                .call(
                    method,
                    path,
                    private,
                    json!({"phone":"+1000","message":"fixture","device":"reader"}),
                )
                .await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
            assert_eq!(value["code"], "sms_store_unavailable", "{path}");
            assert_eq!(value["retryable"], false);
        }
        let (status, body) = f
            .call(
                Method::POST,
                "/api/internal/device-send/claim",
                false,
                json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "route_not_found");
        assert_eq!(
            f.call(
                Method::GET,
                "/api/v2/migration-readiness",
                false,
                Value::Null
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            f.call(
                Method::GET,
                "/api/multistandby/conversations",
                false,
                Value::Null
            )
            .await
            .0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn nwait_fault_blocks_inbound_ack_claim_and_exports_without_blocking_other_store_or_cleanup()
     {
        let f = Fixture::new(Some(("multistandby/store-v1.json", b"")));
        for (method, path, private) in [
            (Method::POST, "/api/multistandby/messages/send", false),
            (Method::POST, "/api/multistandby/send-intents/submit", false),
            (Method::POST, "/api/multistandby/start", false),
            (Method::POST, "/api/multistandby/retry", false),
            (Method::GET, "/api/multistandby/profiles", false),
            (Method::GET, "/api/multistandby/messages", false),
        ] {
            let (status, value) = f.call(method, path, private, json!({})).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
            assert_eq!(value["code"], "sms_store_unavailable", "{path}");
        }
        for action in [
            "claim",
            "claim-next",
            "inbound",
            "inbound-event",
            "outcome",
            "queue",
        ] {
            assert!(
                matches!(f.state.multistandby_store.apply(action, json!({"event_id":format!("blocked-{action}")})),
                Err(StoreError::Configuration(code)) if code == "sms_store_unavailable"),
                "{action}"
            );
        }
        let generation = crate::native_vowifi_supervisor::SessionGeneration::from_value(1).unwrap();
        let event = native_incoming_event(generation);
        let result = crate::ims_sms_transactions::archive_native_sms_event_to(
            f.root.clone(),
            "unused-nwait-subscription",
            generation,
            &event,
            1_800_000_000_123,
            &crate::ims_sms_transactions::NativeSmsArchive::Nwait {
                store: f.state.multistandby_store.clone(),
                profile_id: "A".into(),
            },
        )
        .await;
        // No NativeSmsCommit exists, so no RP-ACK can be authorized.
        assert!(matches!(result, Err("sms_store_unavailable")));
        assert_eq!(
            fs::read(f.root.join("multistandby/store-v1.json")).unwrap(),
            b""
        );
        let (status, value) = f
            .call(
                Method::POST,
                "/api/internal/reader-business",
                true,
                json!({"action":"acquire","producer_id":"fixture","sequence":1,
                "token":"business-fixture-token-0000000000000","role":"multistandby",
                "reader":"reader-a","reader_generation":1}),
            )
            .await;
        // The retired compatibility action is rejected before store admission;
        // native submission and archive failure checks above remain mandatory.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["code"], "invalid_request");
        for source in ["all", "nwait"] {
            let (status, value) = f
                .call(
                    Method::POST,
                    "/api/v2/sms-export",
                    false,
                    json!({"source":source}),
                )
                .await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(value["code"], "sms_store_unavailable");
        }
        let (status, value) = f.call(Method::POST, "/api/internal/device-send/prepare", true,
            json!({"client_request_id":"good","phone":"+1000","message":"fixture","device":"reader","binding":"route"})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["state"], "prepared");
        for path in [
            "/api/multistandby/stop",
            "/api/multistandby/report",
            "/api/multistandby/operations/x",
        ] {
            assert!(crate::sms_store_rejection(path, &f.state).is_none());
        }
        let (status, value) = f
            .call(
                Method::POST,
                "/api/internal/reader-business",
                true,
                json!({"action":"release","producer_id":"fixture","sequence":1,"token":"business-fixture-token-0000000000000","role":"multistandby","reader":"reader-a","reader_generation":1}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["code"], "invalid_request");
    }

    #[tokio::test]
    async fn first_export_detects_live_source_fault_and_latches_claims_while_device_export_works() {
        for source in ["nwait", "all"] {
            for damage in [
                "unlink",
                "",
                "null",
                "[]",
                "[{broken",
                "[{\"revision\":0,\"schema_version\":2}]",
            ] {
                let f = Fixture::new(None);
                let path = f.root.join("multistandby/store-v1.json");
                let original = fs::read(&path).unwrap();
                if damage == "unlink" {
                    fs::remove_file(&path).unwrap();
                } else {
                    fs::write(&path, damage).unwrap();
                }
                assert_eq!(f.state.multistandby_store.status().state, "ready");
                let (status, body) = f
                    .call(
                        Method::POST,
                        "/api/v2/sms-export",
                        false,
                        json!({"source":source}),
                    )
                    .await;
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{source}/{damage}");
                assert_eq!(body["code"], "sms_store_unavailable");
                assert_eq!(
                    f.state.multistandby_store.status().reason,
                    Some("store_corrupt")
                );
                assert!(
                    matches!(f.state.multistandby_store.apply("claim-next", json!({
                    "event_id":"latched-claim", "profile_id":"A", "attempt_token":"latched-attempt"
                })), Err(StoreError::Configuration(code)) if code == "sms_store_unavailable")
                );
                if damage == "unlink" {
                    assert!(!path.exists());
                } else {
                    assert_eq!(fs::read(&path).unwrap(), damage.as_bytes());
                }
                fs::write(
                    f.root.join("sms-archive.json"),
                    br#"[{"direction":"in","sender":"fixture","content":"independent"}]"#,
                )
                .unwrap();
                let (status, body) = f
                    .call(
                        Method::POST,
                        "/api/v2/sms-export",
                        false,
                        json!({"source":"device"}),
                    )
                    .await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(body["message_count"], 1);
                assert_eq!(f.state.device_send_store.status().state, "ready");
                let route = format!("/api/v2/sms-export/{}", body["token"].as_str().unwrap());
                assert_eq!(
                    f.call(Method::DELETE, &route, false, json!({})).await.0,
                    StatusCode::OK
                );
                fs::write(&path, original).unwrap();
                assert_eq!(
                    f.call(
                        Method::POST,
                        "/api/v2/sms-export",
                        false,
                        json!({"source":source})
                    )
                    .await
                    .0,
                    StatusCode::SERVICE_UNAVAILABLE
                );
                let (fresh, _, _) = crate::initialize_sms_stores(&f.root).unwrap();
                assert_eq!(fresh.status().state, "ready");
            }
        }
    }

    #[tokio::test]
    async fn runtime_read_fault_latches_through_external_repair_until_fresh_startup() {
        let f = Fixture::new(None);
        let path = f.root.join("multistandby/store-v1.json");
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"[{\"schema_version\":999}]").unwrap();
        assert!(
            !f.call(
                Method::GET,
                "/api/multistandby/messages",
                false,
                Value::Null
            )
            .await
            .0
            .is_success()
        );
        assert_eq!(
            f.state.multistandby_store.status().reason,
            Some("store_corrupt")
        );
        fs::write(&path, &original).unwrap();
        assert_eq!(
            f.call(
                Method::GET,
                "/api/multistandby/messages",
                false,
                Value::Null
            )
            .await
            .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(f.state.multistandby_store.recover().is_err());
        let (fresh, device, _) = crate::initialize_sms_stores(&f.root).unwrap();
        assert_eq!(fresh.status().state, "ready");
        assert_eq!(device.status().state, "ready");
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn io_fault_after_atomic_commit_cannot_return_permission_or_admit_another_mutation() {
        let f = Fixture::new(None);
        let path = f.root.join("injected-ledger.json");
        let archive = GuardedArchive::from(ArchiveStore::new(&path));
        let operation: Result<bool, StoreError> = archive.checked(|| {
            archive
                .archive
                .replace(vec![json!({"state":"submitting","attempt":"fixed"})])?;
            // Model rename succeeding followed by directory fsync failing.
            Err(StoreError::Io(std::io::Error::from_raw_os_error(28)))
        });
        assert!(operation.is_err());
        assert_eq!(archive.status().reason, Some("store_io_failed"));
        let bytes = fs::read(&path).unwrap();
        assert!(
            archive
                .mutate::<()>(|_| panic!("a faulted store must never run this claim"))
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert!(archive.status().restart_required);
    }

    #[test]
    fn queued_operation_cannot_overtake_a_failing_persistence_operation() {
        let f = Fixture::new(None);
        let archive = GuardedArchive::from(ArchiveStore::new(f.root.join("serialized.json")));
        let (entered, admission) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let first = archive.clone();
        let faulting = std::thread::spawn(move || {
            first.checked::<()>(|| {
                entered.send(()).unwrap();
                resume.recv().unwrap();
                Err(StoreError::Io(std::io::Error::from_raw_os_error(28)))
            })
        });
        admission.recv().unwrap();
        let second = archive.clone();
        let queued = std::thread::spawn(move || {
            second.mutate::<()>(|_| panic!("queued claim ran after disk failure"))
        });
        release.send(()).unwrap();
        assert!(faulting.join().unwrap().is_err());
        assert!(queued.join().unwrap().is_err());
        assert_eq!(archive.status().reason, Some("store_io_failed"));
    }

    #[tokio::test]
    async fn fresh_recovery_preserves_prepared_and_accepted_and_never_reissues_submitted_claim() {
        let mut f = Fixture::new(None);
        for id in ["prepared", "unknown", "accepted"] {
            assert_eq!(f.call(Method::POST, "/api/internal/device-send/prepare", true,
                json!({"client_request_id":id,"phone":"+1000","message":id,"device":"reader","binding":"route"})).await.0, StatusCode::OK);
            if id != "prepared" {
                let (_, claim) = f
                    .call(
                        Method::POST,
                        "/api/internal/device-send/claim",
                        true,
                        json!({"client_request_id":id,"binding":"route","attempt":"original"}),
                    )
                    .await;
                assert_eq!(claim["can_submit"], true);
            }
        }
        assert_eq!(f.call(Method::POST, "/api/internal/device-send/finish", true,
            json!({"client_request_id":"accepted","attempt":"original","state":"accepted","segments":1,"archive_confirmed":true})).await.0, StatusCode::OK);
        let path = f.root.join("device-send-ledger.json");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
        let (status, _) = f
            .call(
                Method::POST,
                "/api/internal/device-send/list",
                true,
                json!({}),
            )
            .await;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            f.call(
                Method::POST,
                "/api/internal/device-send/claim",
                true,
                json!({"client_request_id":"prepared","binding":"route","attempt":"new"})
            )
            .await
            .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let (nwait, device, _) = crate::initialize_sms_stores(&f.root).unwrap();
        f.state.multistandby_store = nwait;
        f.state.device_send_store = device;
        for id in ["prepared", "unknown", "accepted"] {
            let (status, record) = f
                .call(
                    Method::POST,
                    "/api/internal/device-send/get",
                    true,
                    json!({"client_request_id":id}),
                )
                .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(record["state"], id);
        }
        let (_, claim) = f
            .call(
                Method::POST,
                "/api/internal/device-send/claim",
                true,
                json!({"client_request_id":"unknown","binding":"route","attempt":"new"}),
            )
            .await;
        assert_eq!(claim["can_submit"], false);
        assert_eq!(claim["attempt"], "original");
    }

    #[test]
    fn filesystem_permission_and_unsafe_path_faults_do_not_claim_ready() {
        let f = Fixture::new(None);
        let path = f.root.join("device-send-ledger.json");
        let original = fs::read(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
        let failed = crate::device_send_store::Store::open(&f.root).unwrap();
        let recovered = failed.recover();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(recovered.is_err());
        assert_eq!(failed.status().reason, Some("store_io_failed"));
        assert!(failed.recover().is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let failed = crate::device_send_store::Store::open(&f.root).unwrap();
        assert!(failed.recover().is_err());
        assert_eq!(failed.status().reason, Some("store_path_unsafe"));
    }

    #[test]
    fn user_validation_conflicts_and_capacity_are_not_disk_faults() {
        let f = Fixture::new(None);
        let archive = GuardedArchive::from(ArchiveStore::new(f.root.join("validation.json")));
        for code in [
            "invalid_request",
            "state_conflict",
            "send_intent_conflict",
            "profile_not_found",
            "ledger_full",
            "store_capacity",
            "device_forwarding_state_conflict",
            "device_forwarding_id_limit",
        ] {
            assert!(
                archive
                    .checked::<()>(|| Err(StoreError::Configuration(code.into())))
                    .is_err()
            );
            assert_eq!(archive.status().state, "ready");
        }
    }

    #[test]
    fn disappeared_file_is_not_recreated_by_a_live_store_and_worker_panic_latches() {
        let f = Fixture::new(None);
        let path = f.root.join("device-send-ledger.json");
        fs::remove_file(&path).unwrap();
        assert!(f.state.device_send_store.recover().is_err());
        assert_eq!(
            f.state.device_send_store.status().reason,
            Some("store_corrupt")
        );
        assert!(!path.exists());

        let archive = GuardedArchive::from(ArchiveStore::new(f.root.join("panic.json")));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = archive.checked::<()>(|| panic!("synthetic archive worker panic"));
        }));
        assert!(caught.is_err());
        assert_eq!(archive.status().reason, Some("store_worker_failed"));
        assert!(
            archive
                .mutate::<()>(|_| panic!("poisoned store admitted a claim"))
                .is_err()
        );
    }

    #[test]
    fn disappeared_file_is_not_recreated_after_guarded_archive_reopen() {
        let f = Fixture::new(None);
        let path = f.root.join("reopened.json");
        let archive = GuardedArchive::from(ArchiveStore::new(&path));
        archive
            .replace(vec![json!({"state":"prepared","id":"durable"})])
            .unwrap();
        drop(archive);
        fs::remove_file(&path).unwrap();

        let reopened = GuardedArchive::from(ArchiveStore::new(&path));
        assert!(reopened.load().is_err());
        assert_eq!(reopened.status().reason, Some("store_corrupt"));
        assert!(
            reopened
                .mutate::<()>(|_| panic!("reopened missing ledger admitted a claim"))
                .is_err()
        );
        assert!(!path.exists());
    }
}

#[cfg(test)]
#[path = "sms_store_health/raw_tests.rs"]
mod raw_tests;
