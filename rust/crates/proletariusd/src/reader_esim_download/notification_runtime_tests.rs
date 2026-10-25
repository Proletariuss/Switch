#[cfg(test)]
mod tests {
    use super::super::*;
    use hyper_util::rt::TokioIo;
    use std::{path::PathBuf, sync::atomic::AtomicU64};
    use tokio::net::{UnixListener, UnixStream};

    #[test]
    fn query_uses_first_decoded_aid_and_trims_only_its_edges() {
        for (query, expected) in [
            ("", None),
            ("other=A000", None),
            ("aid=++&aid=A000", None),
            ("aid=%20%09a000%20", Some(vec![0xa0, 0x00])),
            ("%61id=A000&aid=B000", Some(vec![0xa0, 0x00])),
            ("other=x&aid=A000&aid=B000", Some(vec![0xa0, 0x00])),
        ] {
            assert_eq!(notification_query_aid(query), Ok(expected), "{query}");
        }
        for query in ["aid=A00", "aid=A0+00", "aid=GG", "aid=%FF", "aid=A000=00"] {
            assert_eq!(notification_query_aid(query), Err(()), "{query}");
        }
        assert!(notification_query_aid(&format!("aid={}", "AA".repeat(255))).is_ok());
        assert!(notification_query_aid(&format!("aid={}", "AA".repeat(256))).is_err());
    }

    #[test]
    fn query_matches_go_skipping_invalid_escape_and_semicolon_pairs() {
        for query in [
            "aid=%GG&aid=A000",
            "aid=%&aid=A000",
            "aid=A000;bad&aid=A000",
            "%XXid=B000&aid=A000",
        ] {
            assert_eq!(
                notification_query_aid(query),
                Ok(Some(vec![0xa0, 0x00])),
                "{query}"
            );
        }
        assert_eq!(notification_query_aid("aid=%GG"), Ok(None));
        assert_eq!(notification_query_aid("aid=A000;bad"), Ok(None));
        // A properly encoded semicolon is a value, not a rejected query pair.
        assert_eq!(notification_query_aid("aid=A0%3B00&aid=A000"), Err(()));
    }

    #[test]
    fn candidates_prioritize_discovered_aids_and_deduplicate_static_fallbacks() {
        let static_first = aid_hex(Some(KNOWN_ISDR_APPLICATION_AIDS[0]));
        let discovered = vec![
            "C01234".into(),
            static_first.to_lowercase(),
            "c01234".into(),
            "invalid".into(),
        ];
        let candidates = notification_candidates(None, &discovered);
        assert_eq!(candidates[0], vec![0xc0, 0x12, 0x34]);
        assert_eq!(candidates[1], KNOWN_ISDR_APPLICATION_AIDS[0]);
        assert_eq!(candidates.len(), KNOWN_ISDR_APPLICATION_AIDS.len() + 1);
        for known in KNOWN_ISDR_APPLICATION_AIDS {
            assert_eq!(
                candidates
                    .iter()
                    .filter(|candidate| candidate.as_slice() == *known)
                    .count(),
                1
            );
        }
        assert_eq!(
            notification_candidates(Some(&[0xbb, 0x00]), &discovered),
            vec![vec![0xbb, 0x00]]
        );
    }

    fn item(sequence: i64, aid: &str, address: &str) -> notifications::NotificationItem {
        notifications::NotificationItem {
            sequence_number: sequence,
            event: "install",
            iccid: String::new(),
            address: address.into(),
            aid_hex: aid.into(),
            can_retry: !address.is_empty(),
        }
    }

    #[test]
    fn fanout_preserves_partial_success_deduplicates_and_sorts() {
        let cancelled = AtomicBool::new(false);
        let candidates = vec![vec![1], vec![2], vec![3]];
        let mut visited = Vec::new();
        let result = collect_notification_aids(&candidates, false, &cancelled, |aid| {
            visited.push(aid[0]);
            match aid[0] {
                1 => Ok(vec![
                    item(4, "01", "first.example"),
                    item(1, "01", "one.example"),
                ]),
                2 => Err(RetryNotificationFailure::Failed("AID unavailable".into())),
                _ => Ok(vec![
                    item(4, "03", "first.example"),
                    item(8, "03", "eight.example"),
                ]),
            }
        })
        .unwrap();
        assert_eq!(visited, vec![1, 2, 3]);
        assert_eq!(
            result,
            vec![
                item(8, "03", "eight.example"),
                item(4, "01", "first.example"),
                item(1, "01", "one.example")
            ]
        );
    }

    #[test]
    fn fanout_all_failed_reports_last_error_but_empty_success_is_valid() {
        let candidates = vec![vec![1], vec![2]];
        let cancelled = AtomicBool::new(false);
        assert_eq!(
            collect_notification_aids(&candidates, false, &cancelled, |aid| {
                Err(RetryNotificationFailure::Failed(format!(
                    "failure {}",
                    aid[0]
                )))
            }),
            Err(RetryNotificationFailure::Failed("failure 2".into()))
        );
        assert_eq!(
            collect_notification_aids(&candidates, false, &cancelled, |aid| {
                if aid[0] == 1 {
                    Ok(Vec::new())
                } else {
                    Err(RetryNotificationFailure::Failed("unavailable".into()))
                }
            }),
            Ok(Vec::new())
        );
    }

    #[test]
    fn explicit_aid_does_not_mask_error_or_merge_distinct_card_records() {
        let candidates = vec![vec![1], vec![2]];
        let cancelled = AtomicBool::new(false);
        let mut visits = 0;
        assert_eq!(
            collect_notification_aids(&candidates, true, &cancelled, |_| {
                visits += 1;
                Err(RetryNotificationFailure::Failed(
                    "explicit AID failed".into(),
                ))
            }),
            Err(RetryNotificationFailure::Failed(
                "explicit AID failed".into()
            ))
        );
        assert_eq!(visits, 1);
        let records = vec![item(4, "01", "example.com"), item(4, "01", "example.com")];
        assert_eq!(
            collect_notification_aids(&candidates[..1], true, &cancelled, |_| Ok(records.clone())),
            Ok(records)
        );
    }

    #[test]
    fn fanout_stops_immediately_on_cancellation_or_uncertain_card_state() {
        let candidates = vec![vec![1], vec![2], vec![3]];
        let cancelled = AtomicBool::new(false);
        let mut visits = 0;
        assert_eq!(
            collect_notification_aids(&candidates, false, &cancelled, |_| {
                visits += 1;
                cancelled.store(true, Ordering::Release);
                Ok(vec![item(1, "01", "example.com")])
            }),
            Err(RetryNotificationFailure::Cancelled)
        );
        assert_eq!(visits, 1);
        for terminal in [
            RetryNotificationFailure::Busy,
            RetryNotificationFailure::Cancelled,
            RetryNotificationFailure::Uncertain("delivery may have completed".into()),
        ] {
            let cancelled = AtomicBool::new(false);
            let mut visits = 0;
            let result = collect_notification_aids(&candidates, false, &cancelled, |_| {
                visits += 1;
                if visits == 1 {
                    Ok(Vec::new())
                } else {
                    Err(terminal.clone_for_test())
                }
            });
            assert_eq!(result, Err(terminal));
            assert_eq!(visits, 2);
        }
    }

    // Keep the production failure enum free of a Clone requirement solely
    // for test-table iteration.
    trait FailureCopy {
        fn clone_for_test(&self) -> Self;
    }
    impl FailureCopy for RetryNotificationFailure {
        fn clone_for_test(&self) -> Self {
            match self {
                Self::Busy => Self::Busy,
                Self::Cancelled => Self::Cancelled,
                Self::Uncertain(message) => Self::Uncertain(message.clone()),
                _ => unreachable!(),
            }
        }
    }

    static NEXT_AREA: AtomicU64 = AtomicU64::new(1);
    struct TestArea(PathBuf);
    impl TestArea {
        fn new() -> Self {
            // The audited clean-build supplies a short TMPDIR for Darwin
            // AF_UNIX fixtures even when its source/capsule path is long.
            let root = std::env::temp_dir().join(format!(
                "nr-{}-{}",
                std::process::id(),
                NEXT_AREA.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn socket(&self) -> PathBuf {
            self.0.join("g.sock")
        }
    }
    impl Drop for TestArea {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct ReleaseTrace {
        ended: AtomicBool,
        end_gate: std::sync::Mutex<
            Option<(
                tokio::sync::oneshot::Sender<()>,
                std::sync::mpsc::Receiver<()>,
            )>,
        >,
    }
    struct FakePhysical(Arc<ReleaseTrace>);
    impl ApduTransport for FakePhysical {
        type Error = crate::reader_pcsc::Failure;
        fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
            Ok(vec![0x90, 0])
        }
    }
    impl crate::reader_pcsc::Physical for FakePhysical {
        fn transaction(
            &mut self,
            body: &mut dyn FnMut(&mut dyn ApduTransport<Error = crate::reader_pcsc::Failure>),
        ) -> Result<(), crate::reader_pcsc::Failure> {
            body(self);
            if let Some((started, finish)) = self.0.end_gate.lock().unwrap().take() {
                let _ = started.send(());
                finish.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            self.0.ended.store(true, Ordering::Release);
            Ok(())
        }
        fn close(&mut self) -> Result<(), crate::reader_pcsc::Failure> {
            Ok(())
        }
    }
    struct DirectSupplier(crate::reader_pcsc::LifecycleBinding);
    impl crate::reader_session_lease::NativeSessionSupplier for DirectSupplier {
        fn pre_admit(
            self: Arc<Self>,
            _: String,
            _: crate::reader_reservation::NativePinPolicy,
            _ordinary: Option<Arc<crate::ordinary_vowifi_admission::Claim>>,
            _maintenance: Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
        ) -> crate::reader_session_lease::PreAdmissionFuture {
            Box::pin(async { Ok(8) })
        }
        fn admit(
            self: Arc<Self>,
            binding: crate::reader_reservation::NativePinBinding,
        ) -> crate::reader_session_lease::AdmissionFuture {
            assert_eq!(binding.reader, "Fixture Reader");
            assert_eq!(binding.reader_generation, 8);
            assert_eq!(
                binding.policy,
                crate::reader_reservation::NativePinPolicy::CardOnly
            );
            Box::pin(async { Ok(()) })
        }
        fn snapshot(
            &self,
            binding: &crate::reader_reservation::NativePinBinding,
        ) -> Result<Value, String> {
            Ok(
                json!({"reader":binding.reader,"reader_generation":binding.reader_generation,"session":self.0,
                "identity":{"iccid":"","imsi":"","mcc":"","mnc":""},"notification_aids":["C01234"]}),
            )
        }
        fn finalize(
            self: Arc<Self>,
            _: crate::reader_reservation::NativePinBinding,
            _: Option<NativeCardContext>,
            _: String,
            _: String,
        ) -> crate::reader_session_lease::SupplierFuture {
            Box::pin(async { Err("unexpected finalization".into()) })
        }
    }
    fn native_fixture_authority() -> (
        Arc<crate::reader_reservation::Gate>,
        crate::reader_pcsc::Owner,
        Arc<DirectSupplier>,
        Arc<ReleaseTrace>,
    ) {
        let gate = Arc::new(crate::reader_reservation::Gate::default());
        gate.initialize_business_producer("notification-go-fixture");
        gate.initialize_native_reader_producer("notification-native-fixture");
        gate.test_seed_selection("Fixture Reader", 8);
        let trace = Arc::new(ReleaseTrace::default());
        let opening = trace.clone();
        let resetting = trace.clone();
        let owner = crate::reader_pcsc::Owner::with_factory(
            "notification-native-fixture".into(),
            Arc::new(move |_| Ok(Box::new(FakePhysical(opening.clone())))),
            Arc::new(|| Ok(vec!["Fixture Reader".into()])),
            Arc::new(move |_, _| Ok(Box::new(FakePhysical(resetting.clone())))),
        );
        let supplier = Arc::new(DirectSupplier(owner.native_open("Fixture Reader").unwrap()));
        let erased: Arc<dyn crate::reader_session_lease::NativeSessionSupplier> = supplier.clone();
        gate.set_native_session_supplier(Arc::downgrade(&erased));
        (gate, owner, supplier, trace)
    }
    #[tokio::test]
    async fn cancelled_http_waiter_keeps_lease_until_blocking_worker_finishes() {
        let area = TestArea::new();
        let (gate, owner, _supplier, trace) = native_fixture_authority();
        let lease = Arc::new(
            NativeReaderSessionLease::acquire_for_card_operation(
                &area.socket(),
                Some("fixture"),
                "Fixture Reader",
                gate.clone(),
                owner.clone(),
            )
            .await
            .unwrap(),
        );
        let retained = lease.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = finished.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let request_cancelled = cancelled.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
        let request = tokio::spawn(async move {
            let _guard = CancelNotificationRequest(request_cancelled.clone());
            tokio::spawn(async move {
                let result = run_notification_lease(lease, request_cancelled, move |cancelled| {
                    let _ = started_tx.send(());
                    finish_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                    assert!(cancelled.load(Ordering::Acquire));
                    worker_finished.store(true, Ordering::Release);
                    Err::<(), _>(RetryNotificationFailure::Cancelled)
                })
                .await;
                let _ = completed_tx.send(result);
            })
            .await
            .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert!(cancelled.load(Ordering::Acquire));
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!retained.release_confirmed().await.unwrap());
        assert!(!trace.ended.load(Ordering::Acquire));
        assert!(gate.admit("Fixture Reader").is_err());
        finish_tx.send(()).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), completed_rx)
                .await
                .unwrap()
                .unwrap(),
            Err(RetryNotificationFailure::Cancelled)
        );
        assert!(finished.load(Ordering::Acquire));
        assert!(trace.ended.load(Ordering::Acquire));
        assert!(retained.release_confirmed().await.unwrap());
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn pre_cancelled_request_never_invokes_worker_but_releases_acquired_lease() {
        let area = TestArea::new();
        let (gate, owner, _supplier, trace) = native_fixture_authority();
        let lease = Arc::new(
            NativeReaderSessionLease::acquire_for_card_operation(
                &area.socket(),
                Some("fixture"),
                "Fixture Reader",
                gate.clone(),
                owner.clone(),
            )
            .await
            .unwrap(),
        );
        assert_eq!(
            run_notification_lease::<()>(lease, Arc::new(AtomicBool::new(true)), |_| panic!(
                "cancelled worker must not touch a card"
            ))
            .await,
            Err(RetryNotificationFailure::Cancelled)
        );
        assert!(trace.ended.load(Ordering::Acquire));
        assert!(gate.admit("Fixture Reader").is_ok());
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn unconfirmed_release_prevents_success_response() {
        let area = TestArea::new();
        let (gate, owner, _supplier, trace) = native_fixture_authority();
        let lease = Arc::new(
            NativeReaderSessionLease::acquire_for_card_operation(
                &area.socket(),
                Some("fixture"),
                "Fixture Reader",
                gate.clone(),
                owner.clone(),
            )
            .await
            .unwrap(),
        );
        let (started, start) = tokio::sync::oneshot::channel();
        let (allow_tx, allow_rx) = std::sync::mpsc::channel();
        *trace.end_gate.lock().unwrap() = Some((started, allow_rx));
        lease.set_release_timeout(Duration::from_millis(40));
        let retained = lease.clone();
        let request = tokio::spawn(async move {
            run_notification_lease(lease, Arc::new(AtomicBool::new(false)), |_| Ok(())).await
        });
        start.await.unwrap();
        assert!(matches!(
            request.await.unwrap(),
            Err(RetryNotificationFailure::Uncertain(_))
        ));
        assert!(!retained.release_confirmed().await.unwrap());
        assert!(
            gate.begin_native_pin(
                "Fixture Reader",
                crate::reader_reservation::NativePinPolicy::CardOnly
            )
            .is_err(),
            "uncertain cleanup lost authoritative exclusion"
        );
        allow_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !retained.release_confirmed().await.unwrap() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(trace.ended.load(Ordering::Acquire));
        owner.shutdown().await.unwrap();
    }

    fn state(area: &TestArea) -> ServerState {
        let sms_inbox = crate::message_archive::InboxState::default();
        ServerState {
            call_mode: Default::default(),
            module_call: Default::default(),
            reader_lifecycle: Default::default(),
            multistandby_card: Default::default(),
            multistandby_service: Default::default(),
            app_token: Some("fixture".into()),
            internal_token: "internal-fixture".into(),
            multistandby_store: crate::multistandby_store::Store::open(&area.0.join("multi"))
                .unwrap(),
            multistandby_operations: crate::multistandby_operations::Store::open(&area.0).unwrap(),
            device_send_store: crate::device_send_store::Store::open(&area.0.join("device"))
                .unwrap(),
            forwarding_handoff_store: crate::forwarding_handoff_store::Store::open(&area.0)
                .unwrap(),
            sms_forwarding: crate::sms_forwarding::Service::new(&area.0.join("forwarding")),
            sms_inbox: sms_inbox.clone(),
            usb_sms_receive: crate::test_usb_sms_receiver(
                sms_inbox.clone(),
                area.0.join("sms-archive.json"),
            ),
            call_archive: ArchiveStore::new(area.0.join("call-log.json")),
            call_facts: crate::call_facts::CallFacts::new(
                ArchiveStore::new(area.0.join("call-log.json")),
                "call-producer-fixture".to_owned(),
            ),
            go_socket_path: area.socket(),
            availability: crate::BackendAvailability::new(),
            reader_discovery: Arc::new(Mutex::new(crate::ReaderDiscovery::default())),
            snapshot_instance_id: "notification-test".into(),
            snapshot_revision: Arc::new(AtomicU64::new(0)),
            eap_exchanges: Arc::new(crate::eap_identity::EapExchangeStore::default()),
            modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
            qmi_enhancement: crate::qmi_enhancement::QmiEnhancement::default(),
            network_traffic: crate::network_traffic::Service::default(),
            usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
            reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
            reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
            module_radio: crate::test_module_radio(&area.0),
            native_vowifi_lifecycle: std::sync::Arc::new(
                crate::native_vowifi_session::NativeLifecycle::default(),
            ),
            ims_sms_transactions: crate::ims_sms_transactions::Service::default(),
            multi_runtime_report: Arc::new(Mutex::new(
                crate::multistandby_runtime_report::Journal::default(),
            )),
        }
    }

    async fn public_request(
        state: ServerState,
        token: Option<&str>,
        path: &str,
    ) -> (StatusCode, Value) {
        request_with_selection(state, token, path, None).await
    }

    async fn request_with_selection(
        state: ServerState,
        token: Option<&str>,
        path: &str,
        selection: Option<String>,
    ) -> (StatusCode, Value) {
        let (client, server) = UnixStream::pair().unwrap();
        let server = tokio::spawn(async move {
            if let Some(reader) = selection {
                let service =
                    hyper::service::service_fn(move |request: Request<hyper::body::Incoming>| {
                        let state = state.clone();
                        let reader = reader.clone();
                        async move {
                            Ok::<_, std::convert::Infallible>(
                                list_notifications_for_selection(request, &state, Some(reader))
                                    .await,
                            )
                        }
                    });
                if let Err(error) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(server), service)
                    .await
                {
                    assert!(
                        crate::benign_connection_error_message(&error.to_string()),
                        "{error}"
                    );
                }
                Ok(())
            } else {
                crate::serve_connection(server, state).await
            }
        });
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
            .await
            .unwrap();
        let connection = tokio::spawn(connection);
        let mut request = Request::builder().uri(path).header("host", "localhost");
        if let Some(token) = token {
            request = request.header("x-proletarius-app-token", token);
        }
        let response = sender
            .send_request(request.body(Full::new(Bytes::new())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        drop(sender);
        server.await.unwrap().unwrap();
        if let Err(error) = connection.await.unwrap() {
            // macOS may report ENOTCONN when the peer has already closed
            // after the complete, successfully collected HTTP response.
            assert!(
                error.is_shutdown(),
                "unexpected HTTP connection failure: {error}"
            );
        }
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn public_route_authenticates_then_rejects_no_selection_without_go() {
        let area = TestArea::new();
        let state = state(&area);
        let listener = UnixListener::bind(area.socket()).unwrap();
        let path = "/api/reader/esim/notifications?aid=A000";
        let (status, body) = public_request(state.clone(), None, path).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "authentication_required");
        let (status, body) = public_request(state, Some("fixture"), path).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "reader_esim_unavailable");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn module_operation_observation_uses_local_journal_without_a_physical_owner_or_peer() {
        let _journal = crate::esim_test_support::journal_guard().await;
        for native_managed in [false, true] {
            let area = TestArea::new();
            let state = state(&area);
            if native_managed {
                state.usb_at.claim_native_management().unwrap();
            }
            let listener = UnixListener::bind(area.socket()).unwrap();
            let local = get_operation(&state).await;
            let module = get_module_operation(&state).await;
            assert_eq!(local.status(), StatusCode::OK);
            assert_eq!(module.status(), StatusCode::OK);
            let local = local.into_body().collect().await.unwrap().to_bytes();
            let module = module.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(local, module);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), listener.accept())
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn selected_reader_rejects_bad_aid_before_go_or_hardware_io() {
        let area = TestArea::new();
        let state = state(&area);
        state.reader_discovery.lock().unwrap().reconcile(
            vec![proletarius_reader::ReaderObservation {
                name: "Fixture Reader".into(),
                card: CardPresence::Present,
                in_use: false,
                muted: false,
                atr: Vec::new(),
            }],
            Some("Fixture Reader"),
        );
        // No Go listener or real PC/SC device exists for this fixture.
        let (status, body) = request_with_selection(
            state,
            Some("fixture"),
            "/api/reader/esim/notifications?aid=not-hex",
            Some("Fixture Reader".into()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "invalid_notification");
        assert_eq!(body["retryable"], false);
    }
}
