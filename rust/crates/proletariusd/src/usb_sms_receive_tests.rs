#[cfg(test)]
mod tests {
    use super::super::*;
    use http_body_util::BodyExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::mpsc;

    const PDU: &str = "0000038188F8000862906000000000020041";
    const AT: &str = "2026-10-05T01:02:03.123400000+00:00";
    fn listing(index: u32, pdu: &str) -> String {
        format!("+CMGL: {index},0,,{}\r\n{pdu}\r\n", pdu.len() / 2 - 1)
    }
    fn raw(adopt: bool) -> Value {
        json!({"producer_id":"producer-1","mode":"usb","adopt":adopt,"poll_interval_ms":8000,"captured_at":AT,"session_id":"usb-handle-1","storages":[{"storage":"SM","response":format!("{}OK\r\n",listing(7,PDU)),"error_code":""},{"storage":"ME","response":"OK\r\n","error_code":""}]})
    }
    fn decoded(value: Value) -> RawSnapshot {
        serde_json::from_value(value).unwrap()
    }
    struct Pending {
        reply: FixtureSnapshotReply,
    }
    struct Fixture {
        directory: PathBuf,
        service: Service,
        requests: mpsc::UnboundedReceiver<FixtureSnapshotReply>,
    }
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    impl Fixture {
        async fn new() -> Self {
            let switch = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .unwrap();
            let directory = switch.join("build/runtime/usb-sms-tests").join(format!(
                "{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&directory).unwrap();
            let (send, requests) = mpsc::unbounded_channel();
            let availability = BackendAvailability::new();
            availability.mark_ready();
            let mut service = Service::new(
                ArchiveStore::new(directory.join("sms.json")),
                InboxState::default(),
                crate::usb_at::Owner::new("producer-1".into()),
                availability,
                "producer-1".into(),
            );
            Arc::get_mut(&mut service.inner).unwrap().source = SnapshotSource::Fixture(send);
            Self {
                directory,
                service,
                requests,
            }
        }
        async fn pending(&mut self) -> Pending {
            Pending {
                reply: tokio::time::timeout(Duration::from_secs(3), self.requests.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            }
        }
        async fn collect(&mut self, value: Value) -> Result<usize, Failure> {
            let inner = self.service.inner.clone();
            let identity = {
                let state = inner.state.lock().unwrap();
                state.epoch
            };
            let producer = inner.state.lock().unwrap().producer_id.clone();
            let task = tokio::spawn(async move { collect_once(&inner, identity, &producer).await });
            self.pending()
                .await
                .reply
                .send((StatusCode::OK, value))
                .unwrap();
            task.await.unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
    async fn until(mut predicate: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !predicate() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn usb_projection_preserves_sm_first_dedup_error_items_and_binary_text() {
        let mut value = raw(false);
        value["storages"][1]["response"] = json!(format!("{}OK\r\n", listing(19, PDU)));
        let snapshot = decoded(value);
        validate_snapshot(&snapshot).unwrap();
        let messages = project_snapshot(&snapshot).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["sender"], "888");
        assert_eq!(messages[0]["content"], "A");
        assert_eq!(messages[0]["module_storage"], "SM");
        assert_eq!(messages[0]["module_index"], 7);
        assert_eq!(messages[0]["device"], "dji");
        assert_eq!(messages[0]["direction"], "in");
        let error = project_storage(
            "+CMGL: 9,0,,1\r\n00\r\nOK\r\n",
            "ME",
            "2026-10-05T00:00:00Z",
        )
        .unwrap();
        assert_eq!(error[0]["sender"], "PDU");
        assert_eq!(error[0]["content"], "[短信解析失败] PDU too short\n00");
        assert!(error[0].get("module_index").is_none());
        assert!(error[0].get("device").is_none());
        let binary = "0000038188F800046290600000000004DEADBEEF";
        let message = project_storage(&listing(11, binary), "ME", AT).unwrap();
        assert_eq!(message[0]["content"], "[二进制数据]\nraw=deadbeef");
    }

    #[test]
    fn usb_partial_storage_failure_and_multipart_boundaries_match_existing_policy() {
        let mut value = raw(false);
        value["storages"][0]["response"] = json!("");
        value["storages"][0]["error_code"] = json!("read_failed");
        let snapshot = decoded(value.clone());
        validate_snapshot(&snapshot).unwrap();
        assert!(
            project_snapshot(&snapshot).unwrap().is_empty(),
            "successful empty ME remains success"
        );
        value["storages"][1]["response"] = json!("");
        value["storages"][1]["error_code"] = json!("select_failed");
        assert_eq!(
            project_snapshot(&decoded(value)).unwrap_err().code,
            "usb_sms_storage_failed"
        );
        let a = "0040038188F800006290600000000008050003C9020182";
        let b = "0040038188F800006290700000000008050003C9020284";
        let mut value = raw(false);
        value["storages"][0]["response"] = json!(format!("{}OK\r\n", listing(7, a)));
        value["storages"][1]["response"] = json!(format!("{}OK\r\n", listing(8, b)));
        assert!(
            project_snapshot(&decoded(value.clone()))
                .unwrap()
                .is_empty(),
            "fragments from distinct memories must not combine"
        );
        value["storages"][0]["response"] =
            json!(format!("{}{}OK\r\n", listing(8, b), listing(7, a)));
        let joined = project_snapshot(&decoded(value)).unwrap();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0]["content"], "AB");
        assert_eq!(joined[0]["module_index"], 7);
    }

    #[tokio::test]
    async fn usb_real_pdu_projection_never_forwards_fragments_through_archive_or_preview() {
        const PART_A: &str = "0040038188F800006290600000000008050003C9020182";
        const PART_B: &str = "0040038188F800006290700000000008050003C9020284";
        let partial_listing = format!("{}OK\r\n", listing(7, PART_A));
        let complete_listing = format!("{}{}OK\r\n", listing(8, PART_B), listing(7, PART_A));

        // Establish the actual decoder contract first: concat metadata exists
        // inside codec records, but incomplete records have no display text.
        let decoded = crate::sms_codec::decode_stored_snapshot_response(&partial_listing).unwrap();
        assert!(decoded["records"][0]["decoded"]["concatenation"].is_object());
        assert_eq!(decoded["records"][0]["decoded"]["complete"], false);
        assert!(decoded["records"][0]["decoded"]["text"].is_null());

        for adopt in [false, true] {
            let mut fixture = Fixture::new().await;
            let mut snapshot = raw(adopt);
            snapshot["storages"][0]["response"] = json!(partial_listing);
            assert_eq!(fixture.collect(snapshot.clone()).await.unwrap(), 0);
            assert!(fixture.service.inner.archive.load().unwrap().is_empty());
            assert!(
                fixture
                    .service
                    .inner
                    .inbox
                    .usb_snapshot()
                    .unwrap()
                    .is_empty()
            );
            assert!(
                crate::message_archive::forwarding_owned_list_from_store_for_test(
                    &fixture.service.inner.inbox,
                    fixture.service.inner.archive.clone()
                )
                .await
                .unwrap()
                .is_empty(),
                "partial PDU escaped through adopt={adopt}"
            );

            snapshot["storages"][0]["response"] = json!(complete_listing);
            // collect_once is the production archive-first/publish-preview
            // coordinator. Repeating its complete listing must expose exactly
            // one AB to the real forwarding-source projection in either mode.
            for _ in 0..2 {
                assert_eq!(fixture.collect(snapshot.clone()).await.unwrap(), 1);
                let preview = fixture.service.inner.inbox.usb_snapshot().unwrap();
                assert_eq!(preview.len(), 1);
                assert_eq!(preview[0]["content"], "AB");
                assert!(preview[0].get("concat").is_none());
                let archived = fixture.service.inner.archive.load().unwrap();
                assert_eq!(archived.len(), usize::from(adopt));
                if adopt {
                    assert_eq!(archived[0]["content"], "AB");
                    assert!(archived[0].get("concat").is_none());
                }
                let forwarding = crate::message_archive::forwarding_owned_list_from_store_for_test(
                    &fixture.service.inner.inbox,
                    fixture.service.inner.archive.clone(),
                )
                .await
                .unwrap();
                assert_eq!(
                    forwarding.len(),
                    1,
                    "archive/preview duplicated adopt={adopt}"
                );
                assert_eq!(forwarding[0]["content"], "AB");
                assert!(forwarding[0].get("concat").is_none());
            }

            // A later storage scan missing one segment cannot reintroduce a
            // raw fragment alongside the retained complete preview/archive.
            snapshot["storages"][0]["response"] = json!(format!("{}OK\r\n", listing(8, PART_B)));
            assert_eq!(fixture.collect(snapshot).await.unwrap(), 1);
            let forwarding = crate::message_archive::forwarding_owned_list_from_store_for_test(
                &fixture.service.inner.inbox,
                fixture.service.inner.archive.clone(),
            )
            .await
            .unwrap();
            assert_eq!(forwarding.len(), 1);
            assert_eq!(forwarding[0]["content"], "AB");
        }
    }

    #[test]
    fn usb_snapshot_validation_and_timestamp_projection_reject_invalid_boundaries() {
        for change in [
            "producer",
            "interval",
            "time",
            "session",
            "order",
            "both",
            "neither",
            "oversize",
            "incomplete",
        ] {
            let mut value = raw(false);
            match change {
                "producer" => value["producer_id"] = json!(""),
                "interval" => value["poll_interval_ms"] = json!(0),
                "time" => value["captured_at"] = json!("invalid"),
                "session" => value["session_id"] = json!(""),
                "order" => value["storages"][0]["storage"] = json!("ME"),
                "both" => value["storages"][0]["error_code"] = json!("read_failed"),
                "neither" => value["storages"][0]["response"] = json!(""),
                "oversize" => {
                    value["storages"][0]["response"] = json!("x".repeat(MAX_STORAGE_BYTES + 1))
                }
                "incomplete" => value["storages"][0]["response"] = json!("+CMGL: 7,0,,1\r\n00\r\n"),
                _ => unreachable!(),
            }
            assert!(validate_snapshot(&decoded(value)).is_err(), "{change}");
        }
        assert_eq!(normalized_time(AT).unwrap(), "2026-10-05T01:02:03.1234Z");
        assert_eq!(
            normalized_time("2026-10-05T01:02:03+08:00").unwrap(),
            "2026-10-05T01:02:03+08:00"
        );
        let mut decoded = json!({"service_center_time":{"year":2026,"month":2,"day":30,"hour":0,"minute":0,"second":0,"offset_minutes":0}});
        assert!(sms_timestamp(&decoded, AT).is_none());
        decoded["service_center_time"]["day"] = json!(28);
        assert_eq!(sms_timestamp(&decoded, AT).unwrap(), "2026-02-28T00:00:00Z");
    }

    #[test]
    fn usb_otp_projection_uses_the_legacy_ascii_digit_and_whitespace_rules() {
        for (text, want) in [
            ("验证码：123456", "123456"),
            ("123456 is your OTP", "123456"),
            ("Your security\tcode: 442211", "442211"),
            ("call 447123456789", ""),
            ("security\u{a0}code 123456", "123456"),
            ("one\u{a0}time password 123456", ""),
            ("OTP１２３４", ""),
            ("Verification code: 000042", "000042"),
        ] {
            assert_eq!(extract_code(text), want, "{text}");
        }
    }

    #[tokio::test]
    async fn usb_poll_schedule_ignores_go_readiness_and_defaults_to_eight_seconds() {
        let fixture = Fixture::new().await;
        let inner = &fixture.service.inner;
        inner.availability.mark_starting();
        let mut generation = None;
        let mut deadline = Instant::now();
        let before = Instant::now();
        assert!(matches!(
            next_step(inner, &mut generation, &mut deadline),
            Step::Delay
        ));
        assert!(deadline >= before + FIRST_POLL_DELAY);
        let same = deadline;
        inner.availability.mark_ready();
        assert!(matches!(
            next_step(inner, &mut generation, &mut deadline),
            Step::Delay
        ));
        assert_eq!(deadline, same);
        assert_eq!(inner.state.lock().unwrap().interval, Duration::from_secs(8));
        deadline = Instant::now() - Duration::from_millis(1);
        assert!(matches!(
            next_step(inner, &mut generation, &mut deadline),
            Step::Collect { .. }
        ));
        fixture.service.invalidate_backend("producer-2".into());
        assert!(matches!(
            next_step(inner, &mut generation, &mut deadline),
            Step::Delay
        ));
    }

    #[tokio::test]
    async fn usb_refreshes_coalesce_after_current_scan_without_overlapping_hardware_requests() {
        let mut fixture = Fixture::new().await;
        fixture.service.start();
        fixture.service.start();
        let service = fixture.service.clone();
        let first = tokio::spawn(async move { service.refresh().await });
        let running = fixture.pending().await;
        let service = fixture.service.clone();
        let second = tokio::spawn(async move { service.refresh().await });
        let service = fixture.service.clone();
        let third = tokio::spawn(async move { service.refresh().await });
        until(|| fixture.service.inner.state.lock().unwrap().waiters.len() == 2).await;
        assert!(fixture.requests.try_recv().is_err());
        running.reply.send((StatusCode::OK, raw(false))).unwrap();
        assert_eq!(first.await.unwrap().unwrap(), 1);
        let following = fixture.pending().await;
        assert!(!second.is_finished());
        assert!(!third.is_finished());
        following.reply.send((StatusCode::OK, raw(false))).unwrap();
        assert_eq!(second.await.unwrap().unwrap(), 1);
        assert_eq!(third.await.unwrap().unwrap(), 1);
        assert!(fixture.requests.try_recv().is_err());
        fixture.service.shutdown().await.unwrap();
        assert_eq!(fixture.service.inner.inbox.usb_snapshot().unwrap().len(), 1);
        assert!(fixture.service.inner.archive.load().unwrap().is_empty());
    }

    #[tokio::test]
    async fn usb_cancelled_refresh_and_shutdown_wait_for_actual_read_and_archive_commit() {
        let mut fixture = Fixture::new().await;
        fixture.service.start();
        let service = fixture.service.clone();
        let request = tokio::spawn(async move { service.refresh().await });
        let pending = fixture.pending().await;
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        let service = fixture.service.clone();
        let shutdown = tokio::spawn(async move { service.shutdown().await });
        until(|| fixture.service.inner.state.lock().unwrap().stopping).await;
        assert!(
            !shutdown.is_finished(),
            "must not abort the active snapshot on HTTP cancellation"
        );
        pending.reply.send((StatusCode::OK, raw(true))).unwrap();
        shutdown.await.unwrap().unwrap();
        let saved = fixture.service.inner.archive.load().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0]["module_storage"], "SM");
        assert_eq!(saved[0]["module_index"], 7);
        assert!(
            fixture.requests.try_recv().is_err(),
            "no automatic USB cleanup endpoint exists"
        );
    }

    #[tokio::test]
    async fn usb_old_process_response_cannot_publish_or_archive_after_backend_replacement() {
        let mut fixture = Fixture::new().await;
        fixture.service.start();
        let service = fixture.service.clone();
        let request = tokio::spawn(async move { service.refresh().await });
        let pending = fixture.pending().await;
        fixture.service.invalidate_backend("producer-2".into());
        pending.reply.send((StatusCode::OK, raw(true))).unwrap();
        assert_eq!(
            request.await.unwrap().unwrap_err().code,
            "usb_sms_stale_generation"
        );
        assert!(
            fixture
                .service
                .inner
                .inbox
                .usb_snapshot()
                .unwrap()
                .is_empty()
        );
        assert!(fixture.service.inner.archive.load().unwrap().is_empty());
        let service = fixture.service.clone();
        let request = tokio::spawn(async move { service.refresh().await });
        let pending = fixture.pending().await;
        let mut next = raw(true);
        next["producer_id"] = json!("producer-2");
        pending.reply.send((StatusCode::OK, next)).unwrap();
        assert_eq!(request.await.unwrap().unwrap(), 1);
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn usb_adoption_failure_keeps_preview_unchanged_and_retry_is_durable_and_idempotent() {
        let mut fixture = Fixture::new().await;
        let archive = fixture.directory.join("sms.json");
        std::fs::write(&archive, "invalid JSON").unwrap();
        assert_eq!(
            fixture.collect(raw(true)).await.unwrap_err().code,
            "sms_archive_unavailable"
        );
        assert!(
            fixture
                .service
                .inner
                .inbox
                .usb_snapshot()
                .unwrap()
                .is_empty()
        );
        assert!(
            !fixture.service.inbox_snapshot().unwrap()["last_poll_error"]
                .as_str()
                .unwrap()
                .is_empty()
        );
        std::fs::remove_file(&archive).unwrap();
        assert_eq!(fixture.collect(raw(true)).await.unwrap(), 1);
        assert_eq!(fixture.collect(raw(true)).await.unwrap(), 1);
        assert_eq!(fixture.service.inner.archive.load().unwrap().len(), 1);
        assert_eq!(fixture.service.inner.inbox.usb_snapshot().unwrap().len(), 1);
        assert_eq!(
            fixture.service.inbox_snapshot().unwrap()["last_poll_error"],
            ""
        );
    }

    #[tokio::test]
    async fn usb_serial_busy_skip_last_poll_and_absent_keeps_existing_preview() {
        let mut fixture = Fixture::new().await;
        fixture.collect(raw(false)).await.unwrap();
        let before = fixture.service.inbox_snapshot().unwrap()["last_poll"].clone();
        for mode in ["serial", "busy", "absent"] {
            let mut value = raw(false);
            value["mode"] = json!(mode);
            value["storages"] = json!([]);
            value["session_id"] = json!("");
            value["captured_at"] = json!("2026-10-06T00:00:00Z");
            let result = fixture.collect(value).await;
            let status = fixture.service.inbox_snapshot().unwrap();
            if mode == "busy" {
                assert_eq!(result.unwrap_err().code, "usb_sms_busy");
            } else {
                assert_eq!(result.unwrap(), 1);
            }
            assert_eq!(status["messages"].as_array().unwrap().len(), 1);
            if mode == "absent" {
                assert_eq!(status["last_poll"], "2026-10-06T00:00:00Z");
            } else {
                assert_eq!(status["last_poll"], before);
            }
            assert_eq!(status["polling"], mode != "serial");
        }
    }

    #[tokio::test]
    async fn usb_refresh_waiter_queue_is_bounded_without_cancelling_the_active_scan() {
        let mut fixture = Fixture::new().await;
        fixture.service.start();
        let service = fixture.service.clone();
        let first = tokio::spawn(async move { service.refresh().await });
        let running = fixture.pending().await;
        let mut waiters = Vec::new();
        for _ in 0..MAX_REFRESH_WAITERS {
            let service = fixture.service.clone();
            waiters.push(tokio::spawn(async move { service.refresh().await }));
        }
        until(|| fixture.service.inner.state.lock().unwrap().waiters.len() == MAX_REFRESH_WAITERS)
            .await;
        assert_eq!(
            fixture.service.refresh().await.unwrap_err().status,
            StatusCode::TOO_MANY_REQUESTS
        );
        let service = fixture.service.clone();
        let shutdown = tokio::spawn(async move { service.shutdown().await });
        until(|| fixture.service.inner.state.lock().unwrap().stopping).await;
        for waiter in waiters {
            assert_eq!(
                waiter.await.unwrap().unwrap_err().code,
                "usb_sms_unavailable"
            );
        }
        running.reply.send((StatusCode::OK, raw(false))).unwrap();
        assert_eq!(first.await.unwrap().unwrap(), 1);
        shutdown.await.unwrap().unwrap();
        assert!(fixture.requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn usb_prune_during_snapshot_prevents_old_reply_from_repopulating_selected_messages() {
        let mut fixture = Fixture::new().await;
        fixture.collect(raw(false)).await.unwrap();
        let old = fixture.service.inner.inbox.usb_snapshot().unwrap()[0].clone();
        let selected = vec![old["id"].as_str().unwrap().to_owned()];
        let inner = fixture.service.inner.clone();
        let request = tokio::spawn(async move { collect_once(&inner, 0, "producer-1").await });
        let pending = fixture.pending().await;
        let mut arrived = old;
        arrived["content"] = json!("new concurrent arrival");
        arrived["module_index"] = json!(8);
        fixture
            .service
            .inner
            .inbox
            .merge_usb(vec![arrived])
            .unwrap();
        assert_eq!(fixture.service.prune(&selected).unwrap(), 1);
        pending.reply.send((StatusCode::OK, raw(true))).unwrap();
        assert_eq!(request.await.unwrap().unwrap(), 1);
        let current = fixture.service.inner.inbox.usb_snapshot().unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0]["content"], "new concurrent arrival");
        assert!(
            fixture.service.inner.archive.load().unwrap().is_empty(),
            "pruning before archive admission discards the stale snapshot"
        );
    }

    #[tokio::test]
    async fn usb_response_from_wrong_producer_is_rejected_even_without_availability_generation_change()
     {
        let mut fixture = Fixture::new().await;
        let mut response = raw(true);
        response["producer_id"] = json!("previous-process");
        let before = fixture.service.inbox_snapshot().unwrap();
        assert_eq!(
            fixture.collect(response).await.unwrap_err().code,
            "usb_sms_stale_generation"
        );
        assert_eq!(fixture.service.inbox_snapshot().unwrap(), before);
        assert!(fixture.service.inner.archive.load().unwrap().is_empty());
    }

    #[tokio::test]
    async fn usb_replacement_during_accepted_archive_write_keeps_history_but_not_new_epoch_preview()
    {
        let mut fixture = Fixture::new().await;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(fixture.directory.join("sms.json.lock"))
            .unwrap();
        lock.lock().unwrap();
        let inner = fixture.service.inner.clone();
        let request = tokio::spawn(async move { collect_once(&inner, 0, "producer-1").await });
        fixture
            .pending()
            .await
            .reply
            .send((StatusCode::OK, raw(true)))
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            fixture.service.inner.archive_started.notified(),
        )
        .await
        .unwrap();
        assert!(!request.is_finished());
        fixture.service.invalidate_backend("producer-2".into());
        drop(lock);
        assert_eq!(
            request.await.unwrap().unwrap_err().code,
            "usb_sms_stale_generation"
        );
        assert_eq!(
            fixture.service.inner.archive.load().unwrap().len(),
            1,
            "already accepted durable historical observation is retained"
        );
        assert!(
            fixture
                .service
                .inner
                .inbox
                .usb_snapshot()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture.service.inbox_snapshot().unwrap()["last_poll"],
            "0001-01-01T00:00:00Z"
        );
    }

    async fn owner_rpc(owner: crate::usb_at::Owner, action: &str, body: Value) -> Value {
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        use std::convert::Infallible;
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let server_task = tokio::spawn(async move {
            let result = hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    TokioIo::new(server),
                    service_fn(move |request| {
                        let owner = owner.clone();
                        async move {
                            Ok::<_, Infallible>(crate::usb_at::handle(request, owner, false).await)
                        }
                    }),
                )
                .await;
            // macOS socket pairs may report ENOTCONN on final shutdown after
            // the complete successful response has already been consumed.
            if let Err(error) = result {
                assert!(
                    crate::benign_connection_error_message(&error.to_string()),
                    "{error}"
                );
            }
        });
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
            .await
            .unwrap();
        let connection = tokio::spawn(connection);
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("/api/internal/usb-at/{action}"))
            .header("host", "localhost")
            .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
            .unwrap();
        let response = sender.send_request(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let value = serde_json::from_slice(&body).unwrap();
        drop(sender);
        let _ = connection.await;
        server_task.await.unwrap();
        value
    }

    #[tokio::test]
    async fn native_owner_collects_archives_and_honors_inhibit_without_go_callback() {
        use crate::usb_at_transport::{Transport, TransportError};
        use std::sync::atomic::AtomicUsize;
        struct Card {
            commands: Arc<Mutex<Vec<String>>>,
            closed: Arc<AtomicUsize>,
            storage: String,
        }
        impl Transport for Card {
            fn checked_close(
                self: Box<Self>,
            ) -> Result<
                crate::usb_at_transport::CloseDisposition,
                crate::usb_at_transport::TransportError,
            > {
                // Explicit fixture release; Drop barriers must finish before success.
                drop(self);
                Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
            }

            fn command(
                &mut self,
                command: &str,
                follow_up: Option<&[u8]>,
                timeout: Duration,
                max_bytes: usize,
            ) -> Result<String, TransportError> {
                assert!(follow_up.is_none());
                assert!(!timeout.is_zero() && timeout <= Duration::from_secs(60));
                assert!((1..=MAX_STORAGE_BYTES).contains(&max_bytes));
                self.commands.lock().unwrap().push(command.into());
                if command.starts_with("AT+CPMS=") {
                    self.storage = if command.contains("\"SM\"") {
                        "SM"
                    } else {
                        "ME"
                    }
                    .into();
                }
                Ok(if command == "AT+QCCID" {
                    "+QCCID: 8988212345678901234\r\nOK\r\n".into()
                } else if command == "AT+CMGL=4" && self.storage == "SM" {
                    format!("{}OK\r\n", listing(7, PDU))
                } else {
                    "OK\r\n".into()
                })
            }
            fn description(&self) -> String {
                "USB AT fixture".into()
            }
        }
        impl Drop for Card {
            fn drop(&mut self) {
                self.closed.fetch_add(1, Ordering::SeqCst);
            }
        }
        let commands = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicUsize::new(0));
        let factory_commands = commands.clone();
        let factory_closed = closed.clone();
        let owner = crate::usb_at::Owner::with_factory(
            "producer-1".into(),
            Arc::new(move || {
                Ok(Box::new(Card {
                    commands: factory_commands.clone(),
                    closed: factory_closed.clone(),
                    storage: String::new(),
                }))
            }),
        );
        let mut fixture = Fixture::new().await;
        owner
            .set_adoption(crate::sms_adopt::Store::new(&fixture.directory), true)
            .await
            .unwrap();
        Arc::get_mut(&mut fixture.service.inner).unwrap().source =
            SnapshotSource::Native(owner.clone());
        owner_rpc(owner.clone(), "configure", json!({"producer_id":"producer-1", "action":"sms", "config_seq":1, "serial":false, "poll_interval_ms":8000})).await;
        owner_rpc(owner.clone(), "configure", json!({"producer_id":"producer-1", "action":"inhibit", "token":"vowifi-session", "inhibit_seq":1, "enabled":true})).await;
        assert_eq!(
            collect_once(&fixture.service.inner, 0, "producer-1")
                .await
                .unwrap_err()
                .code,
            "usb_sms_busy"
        );
        assert!(commands.lock().unwrap().is_empty());
        owner_rpc(owner.clone(), "configure", json!({"producer_id":"producer-1", "action":"inhibit", "token":"vowifi-session", "inhibit_seq":1, "enabled":false})).await;
        assert_eq!(
            collect_once(&fixture.service.inner, 0, "producer-1")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            commands.lock().unwrap().as_slice(),
            [
                "AT+QCCID",
                "AT+CMGF=0",
                "AT+CPMS=\"SM\",\"SM\",\"SM\"",
                "AT+CMGL=4",
                "AT+CPMS=\"ME\",\"ME\",\"ME\"",
                "AT+CMGL=4",
                "AT+QCCID"
            ]
        );
        let messages = fixture.service.inner.archive.load().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "A");
        assert_eq!(messages[0]["sender"], "888");
        assert_eq!(fixture.service.inner.inbox.usb_snapshot().unwrap().len(), 1);
        assert!(
            fixture.requests.try_recv().is_err(),
            "the native path must never request the old Go snapshot"
        );
        assert_eq!(closed.load(Ordering::SeqCst), 0);
        owner.shutdown().await.unwrap();
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }

    #[derive(Default)]
    struct PhysicalTrace {
        commands: Mutex<Vec<String>>,
        block: Mutex<Option<String>>,
        released: std::sync::Condvar,
        entered: Notify,
        deleted: std::sync::atomic::AtomicBool,
        opened: std::sync::atomic::AtomicUsize,
        closed: std::sync::atomic::AtomicUsize,
    }
    impl PhysicalTrace {
        fn hold(&self, command: &str) {
            *self.block.lock().unwrap() = Some(command.into());
        }
        fn release(&self) {
            self.block.lock().unwrap().take();
            self.released.notify_all();
        }
        fn release_and_hold(&self, command: &str) {
            *self.block.lock().unwrap() = Some(command.into());
            self.released.notify_all();
        }
        async fn entered(&self) {
            tokio::time::timeout(Duration::from_secs(5), self.entered.notified())
                .await
                .unwrap();
        }
        fn count(&self, command: &str) -> usize {
            self.commands
                .lock()
                .unwrap()
                .iter()
                .filter(|s| *s == command)
                .count()
        }
    }
    struct PhysicalCard {
        trace: Arc<PhysicalTrace>,
        storage: String,
    }
    impl crate::usb_at_transport::Transport for PhysicalCard {
        fn checked_close(
            self: Box<Self>,
        ) -> Result<
            crate::usb_at_transport::CloseDisposition,
            crate::usb_at_transport::TransportError,
        > {
            // Explicit fixture release; Drop barriers must finish before success.
            drop(self);
            Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
        }

        fn command(
            &mut self,
            command: &str,
            follow_up: Option<&[u8]>,
            timeout: Duration,
            _: usize,
        ) -> Result<String, crate::usb_at_transport::TransportError> {
            assert!(follow_up.is_none() && !timeout.is_zero());
            self.trace.commands.lock().unwrap().push(command.into());
            let mut gate = self.trace.block.lock().unwrap();
            if gate.as_deref() == Some(command) {
                self.trace.entered.notify_one();
                while gate.as_deref() == Some(command) {
                    gate = self.trace.released.wait(gate).unwrap();
                }
            }
            drop(gate);
            if command.starts_with("AT+CPMS=") {
                self.storage = if command.contains("\"SM\"") {
                    "SM"
                } else {
                    "ME"
                }
                .into();
            }
            let count =
                usize::from(self.storage == "SM" && !self.trace.deleted.load(Ordering::SeqCst));
            Ok(match command {
                "AT+QCCID" => "+QCCID: 8988212345678901234\r\nOK\r\n".into(),
                "AT+CPMS?" => format!(
                    "+CPMS: \"{}\",{count},20,\"{}\",{count},20,\"{}\",{count},20\r\nOK\r\n",
                    self.storage, self.storage, self.storage
                ),
                "AT+CMGL=4" if count == 1 => format!("{}OK\r\n", listing(7, PDU)),
                "AT+CMGR=7" => format!("+CMGR: 0,,{}\r\n{PDU}\r\nOK\r\n", PDU.len() / 2 - 1),
                "AT+CMGD=7" => {
                    self.trace.deleted.store(true, Ordering::SeqCst);
                    "OK\r\n".into()
                }
                _ => "OK\r\n".into(),
            })
        }
        fn description(&self) -> String {
            "503 retained USB fixture".into()
        }
    }
    impl Drop for PhysicalCard {
        fn drop(&mut self) {
            self.trace.closed.fetch_add(1, Ordering::SeqCst);
        }
    }
    async fn unready_physical_receiver() -> (Fixture, crate::usb_at::Owner, Arc<PhysicalTrace>) {
        let trace = Arc::new(PhysicalTrace::default());
        let factory = trace.clone();
        let owner = crate::usb_at::Owner::with_factory(
            "producer-1".into(),
            Arc::new(move || {
                factory.opened.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(PhysicalCard {
                    trace: factory.clone(),
                    storage: "SM".into(),
                }))
            }),
        );
        let mut fixture = Fixture::new().await;
        fixture.service.inner.availability.mark_starting();
        owner.claim_native_management().unwrap();
        owner
            .set_adoption(crate::sms_adopt::Store::new(&fixture.directory), true)
            .await
            .unwrap();
        Arc::get_mut(&mut fixture.service.inner).unwrap().source =
            SnapshotSource::Native(owner.clone());
        assert!(owner.sms_poll_binding("producer-1").unwrap().is_none());
        (fixture, owner, trace)
    }
    async fn physical_receiver() -> (Fixture, crate::usb_at::Owner, Arc<PhysicalTrace>) {
        let (fixture, owner, trace) = unready_physical_receiver().await;
        let session = owner.open_native_session().await.unwrap();
        assert!(owner.sms_poll_binding("producer-1").unwrap().is_none());
        owner.commit_native_sms_ready(&session).unwrap();
        (fixture, owner, trace)
    }
    async fn assert_local_usage(service: &Service, expected_count: usize) {
        let response = service.storage_usage_response(false).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(
            body,
            json!({"usage":{"LOCAL":{"used":expected_count,"total":1000}}})
        );
    }
    #[tokio::test]
    async fn native_usb_sms_usage_before_device_open_is_local_only_without_hardware_io() {
        let (fixture, owner, trace) = unready_physical_receiver().await;
        crate::message_archive::ingest_messages_to(
            fixture.service.inner.archive.clone(),
            vec![json!({"sender":"888","content":"saved","timestamp":AT})],
        )
        .await
        .unwrap();
        assert_local_usage(&fixture.service, 1).await;
        assert_eq!(trace.opened.load(Ordering::SeqCst), 0);
        assert!(trace.commands.lock().unwrap().is_empty());
        assert!(!owner.native_sms_ready());

        // A stale producer is an error, not an absent-card success.
        owner.replace_producer("producer-2".into()).await.unwrap();
        let response = fixture.service.storage_usage_response(false).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["code"], "usb_sms_unavailable");
        assert!(body.get("usage").is_none());
        assert_eq!(trace.opened.load(Ordering::SeqCst), 0);
        assert!(trace.commands.lock().unwrap().is_empty());
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn native_usb_sms_usage_after_close_and_reopen_stays_local_until_new_bootstrap() {
        let (fixture, owner, trace) = physical_receiver().await;
        collect_once(&fixture.service.inner, 0, "producer-1")
            .await
            .unwrap();
        let previous_session = owner.open_native_session().await.unwrap();
        owner_rpc(
            owner.clone(),
            "close",
            json!({"producer_id":"producer-1","session_id":previous_session,"lease_id":""}),
        )
        .await;
        assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
        let commands = trace.commands.lock().unwrap().clone();
        assert_local_usage(&fixture.service, 1).await;
        assert_eq!(trace.opened.load(Ordering::SeqCst), 1);
        assert_eq!(*trace.commands.lock().unwrap(), commands);

        let reopened = owner.open_native_session().await.unwrap();
        assert_ne!(reopened, previous_session);
        assert!(!owner.native_sms_ready());
        assert_local_usage(&fixture.service, 1).await;
        assert_eq!(trace.opened.load(Ordering::SeqCst), 2);
        assert_eq!(*trace.commands.lock().unwrap(), commands);

        owner.commit_native_sms_ready(&reopened).unwrap();
        let response = fixture.service.storage_usage_response(false).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["usage"]["LOCAL"]["used"], 1);
        assert!(body["usage"].get("SM").is_some());
        assert!(body["usage"].get("ME").is_some());
        assert!(trace.commands.lock().unwrap().len() > commands.len());
        owner.shutdown().await.unwrap();
        let response = fixture.service.storage_usage_response(false).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    async fn configure_receiver(owner: &crate::usb_at::Owner, seq: u64, serial: bool) {
        owner_rpc(owner.clone(),"configure",json!({"producer_id":"producer-1","action":"sms","config_seq":seq,"serial":serial,"poll_interval_ms":8000})).await;
    }
    #[tokio::test]
    async fn native_usb_sms_go_restart_during_real_cmgl_preserves_observation_and_archive() {
        let (fixture, owner, trace) = physical_receiver().await;
        trace.hold("AT+CMGL=4");
        let inner = fixture.service.inner.clone();
        let read = tokio::spawn(async move { collect_once(&inner, 0, "producer-1").await });
        trace.entered().await;
        fixture.service.inner.availability.mark_ready();
        fixture.service.inner.availability.mark_starting();
        fixture.service.inner.availability.child_started();
        fixture.service.inner.availability.mark_restarting(2);
        fixture.service.inner.availability.mark_failed();
        assert!(!read.is_finished());
        trace.release();
        assert_eq!(read.await.unwrap().unwrap(), 1);
        assert_eq!(fixture.service.inner.archive.load().unwrap().len(), 1);
        let inbox = fixture.service.inbox_snapshot().unwrap();
        assert_eq!(inbox["deletion_senders"], json!(["888"]));
        assert_eq!(inbox["messages"][0]["content"], "A");
        assert!(inbox["messages"][0]["deletion_token"].as_str().is_some());
        assert_eq!(trace.count("AT+CMGL=4"), 2);
        let old_token = inbox["messages"][0]["deletion_token"]
            .as_str()
            .unwrap()
            .to_owned();
        // Same session and same PDU after a new committed SMS configuration
        // must not resurrect the previously issued opaque capability.
        configure_receiver(&owner, 2, false).await;
        collect_once(&fixture.service.inner, 0, "producer-1")
            .await
            .unwrap();
        let refreshed = fixture.service.inbox_snapshot().unwrap();
        assert_ne!(refreshed["messages"][0]["deletion_token"], old_token);
        assert!(
            fixture
                .service
                .execute_public_storage_unleased(PublicAction::Delete {
                    tokens: vec![old_token]
                })
                .await
                .is_err()
        );
        assert_eq!(trace.count("AT+CMGD=7"), 0);

        owner.replace_producer("producer-2".into()).await.unwrap();
        assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
        let inbox = fixture.service.inbox_snapshot().unwrap();
        assert_eq!(inbox["deletion_senders"], json!([]));
        assert!(inbox["messages"][0].get("deletion_token").is_none());
    }
    #[tokio::test]
    async fn native_usb_sms_config_change_during_real_read_cannot_publish_or_archive() {
        let (fixture, owner, trace) = physical_receiver().await;
        trace.hold("AT+CMGL=4");
        let inner = fixture.service.inner.clone();
        let read = tokio::spawn(async move { collect_once(&inner, 0, "producer-1").await });
        trace.entered().await;
        configure_receiver(&owner, 2, true).await;
        trace.release();
        assert!(read.await.unwrap().is_err());
        assert!(fixture.service.inner.archive.load().unwrap().is_empty());
        assert!(
            fixture.service.inbox_snapshot().unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(trace.count("AT+CMGL=4"), 1);
        configure_receiver(&owner, 3, false).await;
        assert_eq!(
            collect_once(&fixture.service.inner, 0, "producer-1")
                .await
                .unwrap(),
            1
        );
        owner.replace_producer("producer-2".into()).await.unwrap();
    }
    #[tokio::test]
    async fn native_usb_sms_durable_commit_keeps_history_but_adoption_aba_revokes_preview() {
        let (fixture, owner, _) = physical_receiver().await;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(fixture.directory.join("sms.json.lock"))
            .unwrap();
        lock.lock().unwrap();
        let inner = fixture.service.inner.clone();
        let read = tokio::spawn(async move { collect_once(&inner, 0, "producer-1").await });
        tokio::time::timeout(
            Duration::from_secs(5),
            fixture.service.inner.archive_started.notified(),
        )
        .await
        .unwrap();
        owner
            .set_adoption(crate::sms_adopt::Store::new(&fixture.directory), false)
            .await
            .unwrap();
        owner
            .set_adoption(crate::sms_adopt::Store::new(&fixture.directory), true)
            .await
            .unwrap();
        assert!(!read.is_finished());
        drop(lock);
        assert_eq!(
            read.await.unwrap().unwrap_err().code,
            "usb_sms_stale_generation"
        );
        assert_eq!(fixture.service.inner.archive.load().unwrap().len(), 1);
        assert!(
            fixture.service.inbox_snapshot().unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        owner.replace_producer("producer-2".into()).await.unwrap();
    }
    #[tokio::test]
    async fn native_usb_sms_config_revocation_during_cmgd_preserves_cleanup_and_no_replay() {
        let (fixture, owner, trace) = physical_receiver().await;
        collect_once(&fixture.service.inner, 0, "producer-1")
            .await
            .unwrap();
        let token = fixture.service.inbox_snapshot().unwrap()["messages"][0]["deletion_token"]
            .as_str()
            .unwrap()
            .to_owned();
        trace.hold("AT+CMGD=7");
        let service = fixture.service.clone();
        let original_token = token.clone();
        let delete = tokio::spawn(async move {
            service
                .execute_public_storage_unleased(PublicAction::Delete {
                    tokens: vec![original_token],
                })
                .await
        });
        trace.entered().await;
        configure_receiver(&owner, 2, true).await;
        let service = fixture.service.clone();
        let stop = tokio::spawn(async move { service.shutdown().await });
        until(|| fixture.service.inner.state.lock().unwrap().stopping).await;
        assert!(!delete.is_finished() && !stop.is_finished());
        trace.release_and_hold("AT+CPMS=\"ME\",\"ME\",\"ME\"");
        trace.entered().await;
        assert!(
            !delete.is_finished() && !stop.is_finished(),
            "shutdown must join physical cleanup after the CMGD reply"
        );
        trace.release();
        let result = delete.await.unwrap().unwrap();
        assert_eq!(result["uncertain"], true);
        assert_eq!(result["complete"], false);
        assert_eq!(result["storage_restored"], true);
        assert_eq!(result["deleted_ids"], json!([]));
        stop.await.unwrap().unwrap();
        assert_eq!(trace.count("AT+CMGD=7"), 1);
        let commands = trace.commands.lock().unwrap();
        let delete_at = commands.iter().position(|c| c == "AT+CMGD=7").unwrap();
        assert!(
            commands[delete_at + 1..]
                .iter()
                .any(|c| c == "AT+CPMS=\"ME\",\"ME\",\"ME\"")
        );
        drop(commands);
        assert!(
            fixture
                .service
                .execute_public_storage_unleased(PublicAction::Delete {
                    tokens: vec![token]
                })
                .await
                .is_err()
        );
        assert_eq!(trace.count("AT+CMGD=7"), 1);
        owner.replace_producer("producer-2".into()).await.unwrap();
    }
    #[tokio::test]
    async fn native_usb_sms_usage_after_configuration_change_is_not_published() {
        let (fixture, owner, trace) = physical_receiver().await;
        collect_once(&fixture.service.inner, 0, "producer-1")
            .await
            .unwrap();
        trace.hold("AT+CPMS?");
        let service = fixture.service.clone();
        let query = tokio::spawn(async move { service.storage_usage_response(false).await });
        trace.entered().await;
        configure_receiver(&owner, 2, true).await;
        trace.release();
        let response = query.await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["code"], "usb_sms_stale_generation");
        assert!(body.get("usage").is_none());
        owner.replace_producer("producer-2".into()).await.unwrap();
    }
}
