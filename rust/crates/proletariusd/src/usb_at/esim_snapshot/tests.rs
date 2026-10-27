use super::*;
use http_body_util::BodyExt;
use proletarius_ccid::bertlv;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Condvar;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Default)]
struct Trace {
    replies: Mutex<VecDeque<String>>,
    unavailable: AtomicBool,
    commands: Mutex<Vec<String>>,
    block: Mutex<Option<String>>,
    entered: Notify,
    released: (Mutex<bool>, Condvar),
}
impl Trace {
    fn overview(&self, iccid: &str) {
        let wrap = |tag: &[u8], value: &[u8]| {
            let mut out = vec![];
            bertlv::encode(tag, value, &mut out).unwrap();
            out
        };
        let mut eid = wrap(&[0xbf, 0x3e], &wrap(&[0x5a], &[0x12; 16]));
        eid.extend([0x90, 0]);
        let mut fields = wrap(&[0x5a], &proletarius_euicc::encode_iccid(iccid).unwrap());
        fields.extend(wrap(&[0x9f, 0x70], &[1]));
        fields.extend(wrap(&[0x90], b"Native profile"));
        let mut profiles = wrap(&[0xbf, 0x2d], &wrap(&[0xa0], &wrap(&[0xe3], &fields)));
        profiles.extend([0x90, 0]);
        let cgla = |bytes: &[u8]| {
            let hex = bytes.iter().map(|b| format!("{b:02X}")).collect::<String>();
            format!("+CGLA: {},\"{}\"\r\nOK\r\n", hex.len(), hex)
        };
        self.replies.lock().unwrap().extend([
            "+CCHO: 1\r\nOK\r\n".into(),
            cgla(&eid),
            cgla(&[0x6a, 0x81]),
            cgla(&[0x6a, 0x81]),
            cgla(&[0x6a, 0x81]),
            cgla(&profiles),
            "OK\r\n".into(),
            "ERROR\r\n".into(),
            "ERROR\r\n".into(),
        ]);
    }
    fn block(&self, command: &str) {
        *self.block.lock().unwrap() = Some(command.into());
        *self.released.0.lock().unwrap() = false;
    }
    fn release(&self) {
        *self
            .released
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        self.released.1.notify_all();
    }
    async fn wait_until_entered(&self) {
        tokio::time::timeout(Duration::from_secs(3), self.entered.notified())
            .await
            .expect("USB eSIM fixture must enter its bounded command gate");
    }
    fn count(&self) -> usize {
        self.commands.lock().unwrap().len()
    }
}
struct Port(Arc<Trace>);
impl Transport for Port {
    fn checked_close(
        self: Box<Self>,
    ) -> Result<crate::usb_at_transport::CloseDisposition, crate::usb_at_transport::TransportError>
    {
        // Explicit fixture release; Drop barriers must finish before success.
        drop(self);
        Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
    }

    fn description(&self) -> String {
        "native-esim-simulated-usb".into()
    }
    fn command(
        &mut self,
        command: &str,
        _: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        self.0.commands.lock().unwrap().push(command.into());
        if self.0.block.lock().unwrap().as_deref() == Some(command) {
            self.0.entered.notify_one();
            let released = self.0.released.0.lock().unwrap();
            let (released, wait) = self
                .0
                .released
                .1
                .wait_timeout_while(released, Duration::from_secs(5), |released| !*released)
                .unwrap();
            assert!(
                !wait.timed_out() && *released,
                "USB eSIM fixture gate timed out"
            );
            self.0.block.lock().unwrap().take();
        }
        if command.starts_with("AT+CCHO=")
            || command.starts_with("AT+CGLA=")
            || command.starts_with("AT+CCHC=")
        {
            if self.0.unavailable.load(Ordering::SeqCst) {
                return Ok("ERROR\r\n".into());
            }
            return Ok(self
                .0
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("expected actual eUICC command"));
        }
        Ok(match command {
            "AT+CFUN?" => "+CFUN: 1\r\nOK\r\n",
            "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n",
            "AT+CEREG?" => "+CEREG: 0,1\r\nOK\r\n",
            "AT+CREG?" => "+CREG: 0,1\r\nOK\r\n",
            "AT+QCCID" => "+QCCID: 8901000000000000000\r\nOK\r\n",
            "AT+CIMI" => "001010000000001\r\nOK\r\n",
            "AT+COPS?" => "+COPS: 0,2,\"00101\",7\r\nOK\r\n",
            _ => "OK\r\n",
        }
        .into())
    }
}
async fn owner() -> (Owner, Arc<Trace>, String) {
    let trace = Arc::new(Trace::default());
    let factory_trace = trace.clone();
    let (owner, session) =
        Owner::native_module_fixture(Arc::new(move || Ok(Box::new(Port(factory_trace.clone())))))
            .await;
    (owner, trace, session)
}

#[tokio::test]
async fn esim_refresh_binding_rejects_identity_and_transport_generation_changes() {
    let (owner, _, _) = owner().await;
    let original = owner.capture_esim_refresh_binding().unwrap();
    assert!(owner.esim_refresh_binding_current(&original));
    owner.inner.state.lock().unwrap().identity_generation += 1;
    assert!(!owner.esim_refresh_binding_current(&original));
    let same_device_new_identity = owner.capture_esim_refresh_binding().unwrap();
    assert!(owner.esim_refresh_binding_current(&same_device_new_identity));
    owner.inner.state.lock().unwrap().epoch += 1;
    assert!(!owner.esim_refresh_binding_current(&same_device_new_identity));
    let same_session_new_epoch = owner.capture_esim_refresh_binding().unwrap();
    owner
        .inner
        .state
        .lock()
        .unwrap()
        .session
        .push_str("-replacement");
    assert!(!owner.esim_refresh_binding_current(&same_session_new_epoch));
}

#[tokio::test]
async fn actual_overview_cache_refresh_transient_failure_and_physical_reopen() {
    let (owner, trace, session) = owner().await;
    trace.overview("8901000000000000000");
    let (first, value) = owner.native_esim_overview(false).await.unwrap();
    assert_eq!(
        value["profiles"][0]["profiles"][0]["iccid"],
        "8901000000000000000"
    );
    let count = trace.count();
    assert_eq!(owner.native_esim_overview(false).await.unwrap().0, first);
    assert_eq!(trace.count(), count);
    trace.overview("8901000000000000001");
    let (second, value) = owner.native_esim_overview(true).await.unwrap();
    assert_ne!(first, second);
    assert_eq!(
        value["profiles"][0]["profiles"][0]["iccid"],
        "8901000000000000001"
    );
    trace.unavailable.store(true, Ordering::SeqCst);
    assert!(owner.native_esim_overview(true).await.is_err());
    trace.unavailable.store(false, Ordering::SeqCst);
    assert!(owner.cached_esim_overview(false).is_none());
    assert!(owner.cached_esim_overview(true).is_some());
    owner
        .close(CloseRequest {
            producer_id: "native-module-test".into(),
            session_id: session,
            lease_id: String::new(),
        })
        .await
        .unwrap();
    assert!(owner.cached_esim_overview(true).is_none());
    trace.overview("8901000000000000000");
    let (new, _) = owner.native_esim_overview(false).await.unwrap();
    assert_ne!(new.session, second.session);
    assert!(!owner.esim_snapshot_current(&second));
}

#[tokio::test]
async fn in_flight_read_cannot_publish_after_mutation_revision_and_identity_change() {
    let (owner, trace, _) = owner().await;
    trace.overview("8901000000000000000");
    trace.block("AT+CCHC=1");
    let task_owner = owner.clone();
    let job = tokio::spawn(async move { task_owner.native_esim_overview(false).await });
    trace.wait_until_entered().await;
    owner.invalidate_esim_overview();
    trace.release();
    assert!(job.await.unwrap().is_err());
    assert!(owner.cached_esim_overview(true).is_none());
    trace.overview("8901000000000000000");
    owner.native_esim_overview(false).await.unwrap();
    owner
        .inner
        .state
        .lock()
        .unwrap()
        .esim_snapshot
        .identity_observed("8901000000000000001");
    assert!(owner.cached_esim_overview(true).is_none());
    trace.overview("8901000000000000001");
    owner.native_esim_overview(false).await.unwrap();
    owner.esim_transaction(|_| Ok(())).await.unwrap();
    assert!(owner.cached_esim_overview(true).is_none());
}

#[tokio::test]
async fn recovery_commit_fence_blocks_a_to_b_revision_change_and_rejects_stale_a() {
    let (owner, trace, _) = owner().await;
    trace.overview("8901000000000000000");
    let (card_a, _) = owner.native_esim_overview(false).await.unwrap();
    let committing_owner = owner.clone();
    let committing_a = card_a.clone();
    let (entered, observed) = std::sync::mpsc::sync_channel(1);
    let (release, wait) = std::sync::mpsc::sync_channel(1);
    let commit = std::thread::spawn(move || {
        committing_owner.with_current_esim_snapshot(&committing_a, |_| {
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
        })
    });
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    // The real refresh commit and physical-session revocation share this lock.
    assert!(owner.inner.state.try_lock().is_err());
    release.send(()).unwrap();
    assert_eq!(commit.join().unwrap(), Some(()));
    owner.invalidate_esim_overview(); // Card B supersedes A's revision.
    let mut ran_stale_commit = false;
    assert!(
        owner
            .with_current_esim_snapshot(&card_a, |_| ran_stale_commit = true)
            .is_none()
    );
    assert!(!ran_stale_commit);
}

#[tokio::test]
async fn actual_health_rejects_inhibit_and_suppression_change_without_mixed_status() {
    let (owner, trace, _) = owner().await;
    trace.overview("8901000000000000000");
    let (key, _) = owner.native_esim_overview(false).await.unwrap();
    owner
        .inner
        .state
        .lock()
        .unwrap()
        .inhibits
        .insert("transition".into(), 1);
    let count = trace.count();
    assert!(owner.native_esim_health_status(&key).await.is_err());
    assert_eq!(trace.count(), count);
    owner.inner.state.lock().unwrap().inhibits.clear();
    let status = owner.native_esim_health_status(&key).await.unwrap();
    assert_eq!(status["reg_status"], 1);
    assert_eq!(status["iccid"], "8901000000000000000");
    trace.block("AT+QCFG=\"usbnet\"");
    let task_owner = owner.clone();
    let task_key = key.clone();
    let job = tokio::spawn(async move { task_owner.native_esim_health_status(&task_key).await });
    trace.wait_until_entered().await;
    owner.set_cellular_suppressed(true);
    trace.release();
    assert!(job.await.unwrap().is_err());
}

struct Fixture {
    root: PathBuf,
    state: crate::ServerState,
    trace: Arc<Trace>,
}
impl Fixture {
    async fn new() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build")
            .join(format!(
                "es501-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&root).unwrap();
        let (usb, trace, session) = owner().await;
        let radio =
            crate::module_radio::Service::native_vowifi_fixture(usb.clone(), session, root.clone())
                .await;
        let inbox = crate::message_archive::InboxState::default();
        let state = crate::ServerState {
            call_mode: Default::default(),
            module_call: Default::default(),
            reader_lifecycle: Default::default(),
            multistandby_card: Default::default(),
            multistandby_service: Default::default(),
            app_token: Some("app-fixture".into()),
            internal_token: "test".into(),
            multistandby_store: crate::multistandby_store::Store::open(&root).unwrap(),
            multistandby_operations: crate::multistandby_operations::Store::open(&root).unwrap(),
            device_send_store: crate::device_send_store::Store::open(&root).unwrap(),
            forwarding_handoff_store: crate::forwarding_handoff_store::Store::open(&root).unwrap(),
            sms_forwarding: crate::sms_forwarding::Service::new(&root),
            sms_inbox: inbox.clone(),
            usb_sms_receive: crate::test_usb_sms_receiver(inbox, root.join("sms-archive.json")),
            usb_at: usb,
            module_radio: radio,
            call_archive: proletarius_store::ArchiveStore::new(root.join("call-log.json")),
            call_facts: crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                "call-producer-fixture".to_owned(),
            ),
            modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
            qmi_enhancement: crate::qmi_enhancement::QmiEnhancement::default(),
            network_traffic: crate::network_traffic::Service::default(),
            go_socket_path: root.join("go.sock"),
            availability: crate::BackendAvailability::new(),
            reader_discovery: Arc::new(Mutex::new(crate::ReaderDiscovery::default())),
            snapshot_instance_id: "public-http-fixture".into(),
            snapshot_revision: Arc::new(AtomicU64::new(0)),
            eap_exchanges: Arc::new(crate::eap_identity::EapExchangeStore::default()),
            reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
            reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
            native_vowifi_lifecycle: std::sync::Arc::new(
                crate::native_vowifi_session::NativeLifecycle::default(),
            ),
            ims_sms_transactions: crate::ims_sms_transactions::Service::default(),
            multi_runtime_report: Arc::new(Mutex::new(
                crate::multistandby_runtime_report::Journal::default(),
            )),
        };
        Self { root, state, trace }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.trace.release();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn body(response: Response<Full<Bytes>>) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn esim_refresh_module_real_adapter_reports_stale_200_as_unknown_without_replay() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let fixture = Fixture::new().await;
    fixture.trace.overview("8901000000000000000");
    let (status, value) =
        body(crate::esim_overview_projection::native_fixture(&fixture.state, true).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["refresh"]["scope"], "module");
    assert_eq!(value["refresh"]["phase"], "completed");
    assert_eq!(value["refresh"]["attempts"], 1);
    let count = fixture.trace.count();
    fixture.trace.unavailable.store(true, Ordering::SeqCst);
    let (status, stale) =
        body(crate::esim_overview_projection::native_fixture(&fixture.state, true).await).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "legacy stale-data projection remains available"
    );
    assert_eq!(stale["refresh"]["outcome"], "unknown");
    assert_eq!(stale["refresh"]["attempts"], 1);
    assert_eq!(stale["refresh"]["retry_performed"], false);
    assert_eq!(
        fixture.trace.count() - count,
        proletarius_euicc::KNOWN_ISDR_APPLICATION_AIDS.len(),
        "one CCHO rejection per candidate; the physical read was not replayed"
    );
    assert!(!fixture.state.module_radio.sim_channel_busy());
}
#[tokio::test]
async fn native_projection_busy_cache_health_and_cancelled_waiter_keep_physical_owner() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let fixture = Fixture::new().await;
    assert!(!fixture.state.go_socket_path.exists());
    fixture.trace.overview("8901000000000000000");
    let (status, value) =
        body(crate::esim_overview_projection::native_fixture(&fixture.state, false).await).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let (status, value) = body(crate::esim_health_projection::handle(&fixture.state).await).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let intent = fixture.state.module_radio.native_vowifi_intent().unwrap();
    intent.prepare().await.unwrap();
    let count = fixture.trace.count();
    let (status, value) =
        body(crate::esim_overview_projection::native_fixture(&fixture.state, true).await).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["capabilities"]["can_switch"], false);
    let (status, value) = body(crate::esim_health_projection::handle(&fixture.state).await).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(fixture.trace.count(), count);
    intent.restore().await.unwrap();
    fixture.trace.overview("8901000000000000001");
    fixture.trace.block("AT+CCHC=1");
    let state = fixture.state.clone();
    let waiter =
        tokio::spawn(
            async move { crate::esim_overview_projection::native_fixture(&state, true).await },
        );
    fixture.trace.wait_until_entered().await;
    waiter.abort();
    let _ = waiter.await;
    assert!(fixture.state.module_radio.sim_channel_busy());
    assert!(fixture.state.module_radio.reserve_esim().await.is_err());
    fixture.trace.release();
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.state.module_radio.sim_channel_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (_, value) = fixture.state.usb_at.cached_esim_overview(false).unwrap();
    assert_eq!(
        value["profiles"][0]["profiles"][0]["iccid"],
        "8901000000000000001"
    );
}

#[tokio::test]
async fn esim_refresh_module_route_cleanup_pending_resumes_original_receipt_without_replay() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let fixture = Fixture::new().await;
    let mut receipts = crate::esim_test_support::observe_refresh_receipts();
    fixture.trace.overview("8901000000000000000");
    fixture.trace.block("AT+CCHC=1");
    let state = fixture.state.clone();
    let response = tokio::spawn(async move {
        crate::esim_test_support::refresh_request(state, "/api/esim?refresh=true").await
    });
    fixture.trace.wait_until_entered().await;
    assert!(fixture.state.module_radio.sim_channel_busy());
    let cleanup = fixture
        .state
        .module_radio
        .borrow_shared_voice_operation("native-module-test", true)
        .unwrap();
    fixture.trace.release();
    let (status, body) = response.await.unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "esim_refresh_cleanup_pending");
    assert_eq!(body["refresh"]["scope"], "module");
    assert_eq!(body["refresh"]["phase"], "pending");
    assert_eq!(body["refresh"]["outcome"], "cleanup_pending");
    assert_eq!(body["refresh"]["attempts"], 1);
    assert_eq!(body["refresh"]["retry_performed"], false);
    assert_eq!(body["retryable"], false);
    assert_eq!(body["recoverable"], false);
    assert!(!body["refresh"]["operation_id"].as_str().unwrap().is_empty());
    let worker_receipt = crate::esim_test_support::next_refresh_receipt(
        &mut receipts,
        body["refresh"]["operation_id"].as_str().unwrap(),
        "pending",
    )
    .await;
    assert_eq!(
        worker_receipt, body["refresh"],
        "the HTTP waiter reports the still-owned refresh intent"
    );
    assert!(
        fixture.state.module_radio.sim_channel_busy(),
        "pending real release cannot clear the owner"
    );
    assert!(fixture.state.module_radio.reserve_esim().await.is_err());
    {
        let commands = fixture.trace.commands.lock().unwrap();
        let primary = commands
            .iter()
            .find(|command| command.starts_with("AT+CCHO="))
            .unwrap();
        assert_eq!(
            commands
                .iter()
                .filter(|command| *command == primary)
                .count(),
            1,
            "the primary ISD-R channel is opened once; optional applet probes are part of that scan"
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.as_str() == "AT+CCHC=1")
                .count(),
            1,
            "one actual profile scan closes its primary channel once"
        );
        assert!(fixture.trace.replies.lock().unwrap().is_empty());
    }
    let count = fixture.trace.count();
    let (status, blocked) =
        crate::esim_test_support::refresh_request(fixture.state.clone(), "/api/esim?refresh=true")
            .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "cached projection remains compatible: {blocked}"
    );
    assert_ne!(blocked["refresh"]["phase"], "completed");
    assert_eq!(
        fixture.trace.count(),
        count,
        "a later HTTP request cannot enter the retained reservation"
    );
    assert!(fixture.state.module_radio.sim_channel_busy());

    // The real pin drop wakes retained cleanup; the original worker publishes
    // its completed receipt without a second CCHO/CGLA/CCHC scan.
    drop(cleanup);
    let completed = crate::esim_test_support::next_refresh_receipt(
        &mut receipts,
        body["refresh"]["operation_id"].as_str().unwrap(),
        "completed",
    )
    .await;
    assert_eq!(completed["operation_id"], body["refresh"]["operation_id"]);
    assert_eq!(completed["scope"], "module");
    assert_eq!(completed["outcome"], "succeeded");
    assert_eq!(completed["attempts"], 1);
    assert_eq!(completed["retry_performed"], false);
    assert!(!fixture.state.module_radio.sim_channel_busy());
    assert_eq!(fixture.trace.count(), count, "cleanup never replays the read");
    let lease = fixture.state.module_radio.reserve_esim().await.unwrap();
    fixture.state.module_radio.release_esim(lease).await.unwrap();
    assert_eq!(fixture.trace.count(), count);
    fixture.state.module_radio.finish_shutdown_receipt().await.unwrap();
}
