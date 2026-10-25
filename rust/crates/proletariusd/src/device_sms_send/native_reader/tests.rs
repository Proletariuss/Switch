use super::*;
use crate::{
    ims_sms_transactions::{NativeReportOwner, Service as Transactions},
    native_ims_calls::{CallBinding, LocalMedia, Media, MediaFuture},
    native_registered_session::{self as registered, Consumers},
    native_sms_consumer::{Consumer, RegisteredConsumer},
    native_sms_originating::{
        Originating, SessionConfig,
        tests::{RegisteredFixture, registered_fixture},
    },
    native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey, SessionGeneration},
};
use proletarius_call::sdp::AudioNegotiation;
use proletarius_sip::{Header, Message, MessageData, Method, Parser, Response};
use std::{
    fs,
    sync::{Mutex, atomic::AtomicUsize},
};
use tokio::sync::{mpsc, watch};

struct NoMedia;
impl Media for NoMedia {
    fn allocate(self: Arc<Self>, _: CallBinding) -> MediaFuture<LocalMedia> {
        Box::pin(async { panic!("SMS must not allocate media") })
    }
    fn configure(self: Arc<Self>, _: CallBinding, _: AudioNegotiation) -> MediaFuture<()> {
        Box::pin(async { panic!("SMS must not configure media") })
    }
    fn start(self: Arc<Self>, _: CallBinding, _: bool) -> MediaFuture<()> {
        Box::pin(async { panic!("SMS must not start media") })
    }
    fn stop(self: Arc<Self>, _: CallBinding) -> MediaFuture<()> {
        Box::pin(async { panic!("SMS must not stop media") })
    }
    fn dtmf(self: Arc<Self>, _: CallBinding, _: char) -> MediaFuture<()> {
        Box::pin(async { panic!("SMS must not send DTMF") })
    }
}
struct Route {
    business_ready: AtomicBool,
    business_calls: AtomicUsize,
    change_during_validation: AtomicBool,
    intent: AtomicBool,
    ready: AtomicBool,
    token: Mutex<String>,
    generation: SessionGeneration,
    controls: Mutex<mpsc::Sender<Control>>,
}
impl ReaderSms for Route {
    fn validate_business(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<(), &'static str>> + Send + '_>,
    > {
        Box::pin(async {
            self.business_calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            if self.change_during_validation.load(Ordering::SeqCst) {
                *self.token.lock().unwrap() = "replacement-after-physical-validation".into();
            }
            if !self.business_ready.load(Ordering::SeqCst) {
                return Err("profile_or_cfun4_changed");
            }
            Ok(())
        })
    }
    fn has_intent(&self) -> bool {
        self.intent.load(Ordering::SeqCst)
    }
    fn capture(&self) -> std::result::Result<NativeSmsBinding, &'static str> {
        if !self.has_intent() || !self.ready.load(Ordering::SeqCst) {
            return Err("not_ready");
        }
        Ok(NativeSmsBinding {
            binding: self.token.lock().unwrap().clone(),
            subscription_id: format!("sub1_{}", "a".repeat(64)),
            generation: self.generation,
            controls: self.controls.lock().unwrap().clone(),
        })
    }
    fn resolve(&self, token: &str) -> std::result::Result<NativeSmsBinding, &'static str> {
        let route = self.capture()?;
        if route.binding != token {
            return Err("stale");
        }
        Ok(route)
    }
}
struct Fixture {
    device: &'static str,
    root: PathBuf,
    service: Service,
    route: Arc<Route>,
    stop: watch::Sender<bool>,
    _lease: watch::Sender<bool>,
    _supervisor: NativeVoWiFiSupervisor,
    wire: RegisteredFixture,
    actor: tokio::task::JoinHandle<crate::native_ims_actor::Exit>,
    dispatcher: tokio::task::JoinHandle<registered::Exit>,
    _notices: mpsc::Receiver<registered::Notice>,
}
impl Fixture {
    async fn new() -> Self {
        Self::new_for(false).await
    }
    async fn new_for(module: bool) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../build/tmp/iteration-485-device-native-sms")
            .join(format!("{}-{id}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let key = ReaderSessionKey::new("fixture-reader", "8901234567890123456").unwrap();
        let permit = supervisor.reserve(key.clone()).unwrap();
        supervisor.mark_running(&permit).unwrap();
        let generation = permit.generation();
        let stop = permit.cancellation_sender();
        let mut wire = registered_fixture(permit.stop_signal()).await;
        let incoming_snapshot = wire
            .session
            .as_ref()
            .unwrap()
            .message_context("sip:configured@ims.example", &[])
            .unwrap();
        let (commands, command_input, events, event_input) = crate::native_ims_actor::channels();
        let (lease, lost) = watch::channel(false);
        let owner = Arc::new(
            NativeReportOwner::new(
                Transactions::default(),
                format!("device-native-sms-owner-{id}"),
            )
            .unwrap(),
        );
        let outgoing = Originating::new(
            owner.clone(),
            SessionConfig {
                generation,
                snapshot: &wire.snapshot,
                minimum_cseq: 74,
                smsc: "+447802002606",
                target: "sip:smsc@ims.example",
                access_network_info: None,
            },
            commands.clone(),
            permit.stop_signal(),
            lost.clone(),
        )
        .unwrap();
        let incoming = RegisteredConsumer::new(
            Consumer::new(
                root.clone(),
                format!("sub1_{}", "a".repeat(64)),
                generation,
                commands.clone(),
                permit.stop_signal(),
                lost.clone(),
                owner,
            ),
            incoming_snapshot,
            None,
            74,
        );
        let calls = crate::native_ims_calls::Service::new(
            generation,
            crate::native_ims_calls::Config {
                local_ip: wire.snapshot.local_ip(),
                port_c: wire.snapshot.local_port(),
                contact: "<sip:authorized@10.0.0.2:5064>".into(),
                from_uri: wire.snapshot.routing().identity().into(),
                preferred_identity: wire.snapshot.routing().identity().into(),
                route: wire.snapshot.routing().routes().join(", "),
                service_routes: wire.snapshot.routing().routes().to_vec(),
                encoded_icsi: String::new(),
                phone_context: String::new(),
                home_domain: "ims.example".into(),
                realm: "ims.example".into(),
                user_agent: "device-native-sms-test".into(),
                security_verify: String::new(),
                pani: String::new(),
                early_media_header: false,
                evs_only: false,
                sms_only: false,
                ussd_service_selector: true,
            },
            commands.clone(),
            Arc::new(NoMedia),
        )
        .unwrap();
        let consumers = Consumers::new(incoming, outgoing, calls).unwrap();
        let session = wire.session.take().unwrap();
        let actor_lost = lost.clone();
        let actor = tokio::spawn(async move {
            crate::native_ims_actor::run(
                proletarius_vowifi::sip_session_runtime::RegisteredSipRuntime::new(session),
                &key,
                permit,
                actor_lost,
                command_input,
                events,
            )
            .await
        });
        let (controls, input) = registered::controls();
        let (notices, notice_input) = mpsc::channel(32);
        let dispatcher = tokio::spawn(consumers.run(event_input, commands, input, notices));
        let route = Arc::new(Route {
            business_ready: AtomicBool::new(true),
            business_calls: AtomicUsize::new(0),
            change_during_validation: AtomicBool::new(false),
            intent: AtomicBool::new(true),
            ready: AtomicBool::new(true),
            token: Mutex::new(format!("epoch-{id}-intent1-generation1")),
            generation,
            controls: Mutex::new(controls),
        });
        let mut service = Service {
            job: None,
            #[cfg(test)]
            shutdown_hooks: Default::default(),
            #[cfg(test)]
            archive_executor: None,
            compatibility_fixture: false,
            store: crate::device_send_store::Store::open(&root).unwrap(),
            archive: ArchiveStore::new(root.join("sms-archive.json")),
            socket: root.join("go-must-not-exist.sock"),
            app_token: None,
            internal_token: "no-go".into(),
            usb_at: crate::usb_at::Owner::unavailable_for_tests("native-reader-test".into()),
            modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
            module_radio: crate::test_module_radio(&root),
            availability: crate::availability::BackendAvailability::new(),
            native_sms: route.clone(),
            native_module_sms: Arc::new(super::super::native_module::RuntimeModuleSms(Arc::new(
                NativeLifecycle::default(),
            ))),
            io_timeout: Duration::from_secs(2),
            claims: Arc::new(Semaphore::new(16)),
        };
        if module {
            service.native_sms = Arc::new(RuntimeReaderSms(Arc::new(NativeLifecycle::default())));
            service.native_module_sms = route.clone();
        }
        Self {
            device: if module { "dji" } else { "reader" },
            root,
            service,
            route,
            stop,
            _lease: lease,
            _supervisor: supervisor,
            wire,
            actor,
            dispatcher,
            _notices: notice_input,
        }
    }
    fn request(&self, id: &str, text: &str) -> SendRequest {
        SendRequest {
            client_request_id: id.into(),
            phone: "+447700900123".into(),
            message: text.into(),
            device: self.device.into(),
            expected_subscription_id: format!("sub1_{}", "a".repeat(64)),
        }
    }
    async fn prepare(&self, id: &str, text: &str) {
        let prepared = self
            .service
            .post(self.request(id, text), true, &AtomicBool::new(false))
            .await
            .unwrap();
        assert_eq!(prepared["can_submit"], true);
        assert_eq!(prepared["device"], self.device);
        assert!(prepared.get("binding").is_none() && prepared.get("attempt").is_none());
    }
    async fn frame(&mut self) -> proletarius_sip::Request {
        let bytes = tokio::time::timeout(Duration::from_secs(5), self.wire.frames.recv())
            .await
            .unwrap()
            .unwrap();
        let (Message::Request(request), used) = Parser::default().parse(&bytes, true).unwrap()
        else {
            panic!("MESSAGE request");
        };
        assert_eq!(used, bytes.len());
        assert_eq!(request.method, Method::MESSAGE);
        assert_eq!(request.uri.to_string(), "sip:smsc@ims.example");
        assert_eq!(
            request.data.headers.get("Route").unwrap().value(),
            "<sip:registered-route@ims.example;lr>"
        );
        request
    }
    async fn respond(&self, request: &proletarius_sip::Request, status: u16) {
        let mut headers = request.data.headers.clone();
        headers.set(Header::new("Content-Length", "0").unwrap());
        self.wire
            .replies
            .as_ref()
            .unwrap()
            .send(Zeroizing::new(
                Message::Response(Response {
                    status,
                    reason: "Fixture".into(),
                    data: MessageData::new("SIP/2.0", headers, vec![]),
                })
                .encode(),
            ))
            .await
            .unwrap();
    }
    async fn record(&self, id: &str) -> Record {
        self.service
            .ledger("get", json!({"client_request_id":id}))
            .await
            .unwrap()
    }
    async fn finish(self) {
        self.stop.send_replace(true);
        let actor = self.actor.await.unwrap();
        assert!(actor.cleanup.is_ok());
        let dispatcher = self.dispatcher.await.unwrap();
        assert!(dispatcher.media_cleanup.is_ok());
        assert!(
            self.wire.frames.is_empty(),
            "SMS was replayed during cleanup"
        );
        self.wire.finish().await;
        assert!(!self.service.socket.exists(), "native branch contacted Go");
        fs::remove_dir_all(self.root).unwrap();
    }
}
fn failure<T>(result: Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected error"),
        Err(error) => error.1["code"].as_str().unwrap().into(),
    }
}

#[tokio::test]
async fn quarantine_preserves_reader_registered_sms_prepare_and_send() {
    let mut f = Fixture::new().await;
    f.service.module_radio = crate::module_radio::Service::new(
        f.root.clone(),
        f.service.usb_at.clone(),
        f.service.modem_actor.clone(),
        crate::BackendAvailability::with_module_recovery(
            crate::module_recovery::Gate::quarantined_for_tests(),
        ),
        "synthetic-quarantined".into(),
    );
    f.prepare("reader-during-module-quarantine", "one").await;
    let service = f.service.clone();
    let body = f.request("reader-during-module-quarantine", "one");
    let send =
        tokio::spawn(async move { service.post(body, false, &AtomicBool::new(false)).await });
    let frame = f.frame().await;
    f.respond(&frame, 200).await;
    assert_eq!(send.await.unwrap().unwrap()["sent"], true);
    assert_eq!(
        f.record("reader-during-module-quarantine").await.state,
        "accepted"
    );
    f.finish().await;
}

#[tokio::test]
async fn real_registered_multipart_claims_once_and_archives_only_the_existing_device_ledger() {
    for module in [false, true] {
        let mut f = Fixture::new_for(module).await;
        let text = "A".repeat(161);
        f.prepare("multipart", &text).await;
        let service = f.service.clone();
        let body = f.request("multipart", &text);
        let send =
            tokio::spawn(async move { service.post(body, false, &AtomicBool::new(false)).await });
        let first = f.frame().await;
        f.respond(&first, 202).await;
        let second = f.frame().await;
        f.respond(&second, 200).await;
        assert_ne!(
            first.data.headers.get("CSeq").unwrap().value(),
            second.data.headers.get("CSeq").unwrap().value()
        );
        assert_eq!(send.await.unwrap().unwrap()["segments"], 2);
        let record = f.record("multipart").await;
        assert_eq!(record.state, "accepted");
        assert!(record.archive_confirmed);
        assert_eq!(
            f.service
                .post(
                    f.request("multipart", &text),
                    false,
                    &AtomicBool::new(false)
                )
                .await
                .unwrap()["sent"],
            true
        );
        let archive: Value =
            serde_json::from_slice(&fs::read(f.root.join("sms-archive.json")).unwrap()).unwrap();
        assert_eq!(archive.as_array().unwrap().len(), 1);
        assert_eq!(archive[0]["send_request_id"], "multipart");
        assert_eq!(
            archive[0]["subscription_id"],
            format!("sub1_{}", "a".repeat(64))
        );
        f.finish().await;
    }
}

#[tokio::test]
async fn real_partial_rejection_preserves_accepted_parts_and_cannot_claim_or_archive_again() {
    for module in [false, true] {
        let mut f = Fixture::new_for(module).await;
        let text = "B".repeat(161);
        f.prepare("partial", &text).await;
        let service = f.service.clone();
        let body = f.request("partial", &text);
        let send =
            tokio::spawn(async move { service.post(body, false, &AtomicBool::new(false)).await });
        let first = f.frame().await;
        f.respond(&first, 200).await;
        let second = f.frame().await;
        f.respond(&second, 403).await;
        assert_eq!(failure(send.await.unwrap()), "sms_send_outcome_unknown");
        let record = f.record("partial").await;
        assert_eq!(record.state, "unknown");
        assert_eq!(record.segments, 1);
        assert!(!record.archive_confirmed);
        assert_eq!(
            failure(
                f.service
                    .post(f.request("partial", &text), false, &AtomicBool::new(false))
                    .await
            ),
            "sms_send_outcome_unknown"
        );
        assert!(!f.root.join("sms-archive.json").exists());
        f.finish().await;
    }
}

#[tokio::test]
async fn native_not_ready_auto_selection_and_subscription_mismatch_never_fall_through_to_go() {
    let f = Fixture::new().await;
    f.route.ready.store(false, Ordering::SeqCst);
    assert_eq!(
        failure(
            f.service
                .post(f.request("not-ready", "one"), true, &AtomicBool::new(false))
                .await
        ),
        "sms_intent_binding_expired"
    );
    let mut auto = f.request("auto", "one");
    auto.device.clear();
    assert_eq!(
        failure(f.service.post(auto, true, &AtomicBool::new(false)).await),
        "sms_device_selection_required"
    );
    f.route.ready.store(true, Ordering::SeqCst);
    let mut wrong = f.request("wrong-sub", "one");
    wrong.expected_subscription_id = "sub1_wrong".into();
    assert_eq!(
        failure(f.service.post(wrong, true, &AtomicBool::new(false)).await),
        "sms_intent_conflict"
    );
    assert!(f.wire.frames.is_empty());
    f.finish().await;
}

#[tokio::test]
async fn restart_epoch_same_numeric_generation_and_renewal_expire_prepared_bindings_without_claim()
{
    for module in [false, true] {
        let f = Fixture::new_for(module).await;
        f.prepare("old", "one").await;
        *f.route.token.lock().unwrap() = "replacement-epoch-intent1-generation1".into();
        assert_eq!(
            f.service.get("client_request_id=old").await.unwrap()["can_submit"],
            false
        );
        assert_eq!(
            failure(
                f.service
                    .post(f.request("old", "one"), false, &AtomicBool::new(false))
                    .await
            ),
            "sms_intent_binding_expired"
        );
        assert_eq!(f.record("old").await.state, "prepared");
        f.prepare("renewing", "two").await;
        f.route.ready.store(false, Ordering::SeqCst);
        assert_eq!(
            failure(
                f.service
                    .post(f.request("renewing", "two"), false, &AtomicBool::new(false))
                    .await
            ),
            "sms_intent_binding_expired"
        );
        f.route.intent.store(false, Ordering::SeqCst);
        assert_eq!(
            f.service.get("client_request_id=old").await.unwrap()["can_submit"],
            false
        );
        assert_eq!(
            failure(
                f.service
                    .post(f.request("old", "one"), false, &AtomicBool::new(false))
                    .await
            ),
            "sms_intent_binding_expired"
        );
        assert!(f.wire.frames.is_empty());
        f.finish().await;
    }
}

#[tokio::test]
async fn cancelled_http_waiter_retains_real_send_finalization_and_never_replays() {
    for module in [false, true] {
        let mut f = Fixture::new_for(module).await;
        f.prepare("cancel", "one").await;
        let service = f.service.clone();
        let body = f.request("cancel", "one");
        let waiter = tokio::spawn(supervise(service, body, false));
        let request = f.frame().await;
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        f.respond(&request, 202).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if f.record("cancel").await.archive_confirmed {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            f.service
                .post(f.request("cancel", "one"), false, &AtomicBool::new(false))
                .await
                .unwrap()["sent"],
            true
        );
        f.finish().await;
    }
}

#[tokio::test]
async fn archive_failure_after_real_acceptance_remains_accepted_and_never_resends() {
    for module in [false, true] {
        let mut f = Fixture::new_for(module).await;
        f.prepare("archive-error", "one").await;
        fs::create_dir(f.root.join("sms-archive.json")).unwrap();
        let service = f.service.clone();
        let body = f.request("archive-error", "one");
        let send =
            tokio::spawn(async move { service.post(body, false, &AtomicBool::new(false)).await });
        let request = f.frame().await;
        f.respond(&request, 202).await;
        assert_eq!(
            failure(send.await.unwrap()),
            "sms_send_archive_outcome_unknown"
        );
        let record = f.record("archive-error").await;
        assert_eq!(record.state, "accepted");
        assert_eq!(record.segments, 1);
        assert!(!record.archive_confirmed);
        assert_eq!(
            failure(
                f.service
                    .post(
                        f.request("archive-error", "one"),
                        false,
                        &AtomicBool::new(false)
                    )
                    .await
            ),
            "sms_send_archive_outcome_unknown"
        );
        f.finish().await;
    }
}

#[tokio::test]
async fn concurrent_real_submit_consumes_one_claim_and_creates_one_archive_record() {
    for module in [false, true] {
        let mut f = Fixture::new_for(module).await;
        f.prepare("race", "one").await;
        let a = f.service.clone();
        let b = f.service.clone();
        let ar = f.request("race", "one");
        let br = f.request("race", "one");
        let first = tokio::spawn(async move { a.post(ar, false, &AtomicBool::new(false)).await });
        let second = tokio::spawn(async move { b.post(br, false, &AtomicBool::new(false)).await });
        let frame = f.frame().await;
        f.respond(&frame, 202).await;
        let a = first.await.unwrap();
        let b = second.await.unwrap();
        assert!(a.is_ok() || b.is_ok());
        for result in [a, b] {
            match result {
                Ok(value) => assert_eq!(value["sent"], true),
                Err(error) => assert_eq!(error.1["code"], "sms_send_outcome_unknown"),
            }
        }
        assert_eq!(f.record("race").await.segments, 1);
        let archive: Value =
            serde_json::from_slice(&fs::read(f.root.join("sms-archive.json")).unwrap()).unwrap();
        assert_eq!(archive.as_array().unwrap().len(), 1);
        f.finish().await;
    }
}

#[tokio::test]
async fn lost_native_start_receipt_keeps_durable_unknown_even_after_real_late_sip_acceptance() {
    for module in [false, true] {
        let mut f = Fixture::new_for(module).await;
        let real = f.route.controls.lock().unwrap().clone();
        let (proxy, mut input) = mpsc::channel(4);
        *f.route.controls.lock().unwrap() = proxy;
        let (batch_sender, batch_receiver) = oneshot::channel();
        let actual = real.clone();
        let proxy_task = tokio::spawn(async move {
            let Some(Control::SendText { input: text, reply }) = input.recv().await else {
                panic!("one start");
            };
            let batch = text.batch_id.clone();
            let (forward, forwarded) = oneshot::channel();
            actual
                .send(Control::SendText {
                    input: text,
                    reply: forward,
                })
                .await
                .unwrap_or_else(|_| panic!("dispatcher alive"));
            assert!(forwarded.await.unwrap().is_ok());
            batch_sender.send(batch).unwrap();
            drop(reply); // This real admitted command's acknowledgement is lost.
            assert!(
                input.recv().await.is_none(),
                "SendText was retried after acknowledgement loss"
            );
        });
        f.prepare("lost", "one").await;
        let service = f.service.clone();
        let body = f.request("lost", "one");
        let send =
            tokio::spawn(async move { service.post(body, false, &AtomicBool::new(false)).await });
        let batch = batch_receiver.await.unwrap();
        let request = f.frame().await;
        assert_eq!(failure(send.await.unwrap()), "sms_send_outcome_unknown");
        f.respond(&request, 202).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (tx, rx) = oneshot::channel();
                real.send(Control::SmsStatus {
                    generation: f.route.generation,
                    batch_id: batch.clone(),
                    reply: tx,
                })
                .await
                .unwrap_or_else(|_| panic!("dispatcher alive"));
                if rx.await.unwrap().unwrap().unwrap().phase == Phase::Accepted {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(f.record("lost").await.state, "unknown");
        assert_eq!(
            failure(
                f.service
                    .post(f.request("lost", "one"), false, &AtomicBool::new(false))
                    .await
            ),
            "sms_send_outcome_unknown"
        );
        assert!(!f.root.join("sms-archive.json").exists());
        proxy_task.abort();
        let _ = proxy_task.await;
        f.finish().await;
    }
}

#[tokio::test]
async fn legacy_reader_prepared_route_is_not_validated_or_executed_through_go_under_native_intent()
{
    let f = Fixture::new().await;
    let _: Record = f.service.ledger("prepare", json!({
        "client_request_id":"go-old","phone":"+447700900123","message":"one","device":"reader",
        "subscription_id":format!("sub1_{}", "a".repeat(64)),"binding":"old-go-reader-token",
    })).await.unwrap();
    assert_eq!(
        f.service.get("client_request_id=go-old").await.unwrap()["can_submit"],
        false
    );
    assert_eq!(
        failure(
            f.service
                .post(f.request("go-old", "one"), false, &AtomicBool::new(false))
                .await
        ),
        "sms_intent_binding_expired"
    );
    assert_eq!(f.record("go-old").await.state, "prepared");
    assert!(f.wire.frames.is_empty());
    f.finish().await;
}

#[tokio::test]
async fn retired_compatibility_binding_expires_before_claim_while_native_reader_stays_independent()
{
    let f = Fixture::new().await;
    let _: Record = f
        .service
        .ledger(
            "prepare",
            json!({
                "client_request_id":"go-dji","phone":"+447700900123","message":"one","device":"dji",
                "subscription_id":"","binding":"compatibility-go-token",
            }),
        )
        .await
        .unwrap();
    let mut request = f.request("go-dji", "one");
    request.device = "dji".into();
    request.expected_subscription_id.clear();
    assert_eq!(
        failure(
            f.service
                .post(request, false, &AtomicBool::new(false))
                .await
        ),
        "sms_intent_binding_expired"
    );
    let record = f.record("go-dji").await;
    assert_eq!(record.state, "prepared");
    assert!(record.attempt.is_empty());
    f.prepare("native-independent", "one").await;
    assert!(f.wire.frames.is_empty());
    f.finish().await;
}

#[tokio::test]
async fn module_not_ready_holds_dji_and_default_selection_without_at_or_go_fallback() {
    let f = Fixture::new_for(true).await;
    f.route.ready.store(false, Ordering::SeqCst);
    for device in ["", "dji"] {
        let mut request = f.request("blocked", "one");
        request.device = device.into();
        assert_eq!(
            failure(f.service.post(request, true, &AtomicBool::new(false)).await),
            "sms_intent_binding_expired"
        );
    }
    f.route.ready.store(true, Ordering::SeqCst);
    let mut request = f.request("default", "one");
    request.device.clear();
    let prepared = f
        .service
        .post(request, true, &AtomicBool::new(false))
        .await
        .unwrap();
    assert_eq!(prepared["device"], "dji");
    assert_eq!(prepared["can_submit"], true);
    assert!(
        f.record("default")
            .await
            .binding
            .starts_with(RUST_MODULE_IMS_BINDING_PREFIX)
    );
    let mut wrong = f.request("wrong-sub", "one");
    wrong.expected_subscription_id = "sub1_other".into();
    assert_eq!(
        failure(f.service.post(wrong, true, &AtomicBool::new(false)).await),
        "sms_intent_conflict"
    );
    f.finish().await;
}

#[tokio::test]
async fn module_physical_business_failure_or_replacement_after_claim_is_unknown_without_send() {
    for change in [false, true] {
        let f = Fixture::new_for(true).await;
        f.prepare("guard", "one").await;
        f.route.business_ready.store(change, Ordering::SeqCst);
        f.route
            .change_during_validation
            .store(change, Ordering::SeqCst);
        assert_eq!(
            failure(
                f.service
                    .post(f.request("guard", "one"), false, &AtomicBool::new(false))
                    .await
            ),
            "sms_send_outcome_unknown"
        );
        let record = f.record("guard").await;
        assert_eq!(record.state, "unknown");
        assert_eq!(record.segments, 0);
        assert_eq!(f.route.business_calls.load(Ordering::SeqCst), 1);
        f.route.business_ready.store(true, Ordering::SeqCst);
        assert_eq!(
            failure(
                f.service
                    .post(f.request("guard", "one"), false, &AtomicBool::new(false))
                    .await
            ),
            "sms_send_outcome_unknown"
        );
        assert_eq!(f.route.business_calls.load(Ordering::SeqCst), 1);
        assert!(f.wire.frames.is_empty());
        f.finish().await;
    }
}

#[tokio::test]
async fn module_go_exit_during_real_sip_wait_does_not_steal_native_claim() {
    let mut f = Fixture::new_for(true).await;
    f.prepare("survive-go", "one").await;
    let service = f.service.clone();
    let request = f.request("survive-go", "one");
    let sender =
        tokio::spawn(async move { service.post(request, false, &AtomicBool::new(false)).await });
    let frame = f.frame().await;
    assert_eq!(f.record("survive-go").await.state, "submitting");
    f.service.store.recover_go_owned().unwrap();
    assert_eq!(f.record("survive-go").await.state, "submitting");
    f.respond(&frame, 202).await;
    assert_eq!(sender.await.unwrap().unwrap()["sent"], true);
    assert!(f.record("survive-go").await.archive_confirmed);
    f.finish().await;
}

#[tokio::test]
async fn module_intent_blocks_prepared_at_and_go_routes_without_consuming_claim() {
    let f = Fixture::new_for(true).await;
    for (id, binding) in [
        ("old-go", "route-old"),
        ("old-at", "rust_usb_at_session:old:7"),
        ("old-modem", "rust_modem_sms_generation:7"),
    ] {
        let record: Record = f.service.ledger("prepare", json!({"client_request_id":id,"phone":"+447700900123","message":"one","device":"dji","subscription_id":format!("sub1_{}", "a".repeat(64)),"binding":binding})).await.unwrap();
        assert_eq!(
            f.service.project(vec![record.clone()]).await[0]["can_submit"],
            false
        );
        assert_eq!(
            failure(f.service.submit(record, &AtomicBool::new(false)).await),
            "sms_intent_binding_expired"
        );
        assert_eq!(f.record(id).await.state, "prepared");
    }
    assert!(f.wire.frames.is_empty());
    f.finish().await;
}

#[tokio::test]
async fn registered_reader_and_actual_serial_ready_still_require_explicit_selection() {
    let serial = crate::device_sms_send::native_serial::tests::Fixture::new(true).await;
    let mut f = Fixture::new().await;
    f.service.modem_actor = serial.state.modem_actor.clone();
    f.service.module_radio = serial.state.module_radio.clone();
    let route = f.route.capture().unwrap();
    assert!(f.route.resolve(&route.binding).is_ok());
    let actor = serial.state.modem_actor.clone();
    assert!(
        tokio::task::spawn_blocking(move || actor.capture_serial_sms_binding())
            .await
            .unwrap()
            .is_ok()
    );
    let mut auto = f.request("both-ready", "one");
    auto.device.clear();
    auto.expected_subscription_id.clear();
    assert_eq!(
        failure(f.service.post(auto, true, &AtomicBool::new(false)).await),
        "sms_device_selection_required"
    );
    assert!(f.wire.frames.is_empty());
    serial.close().await;
    f.finish().await;
}

#[tokio::test]
async fn shutdown_retains_real_registered_send_across_physical_result_and_ims_poll_gap() {
    for stage in ["physical-result", "ims-poll", "archive-failure"] {
        for module in [false, true] {
            let mut f = Fixture::new_for(module).await;
            let id = format!("shutdown-{stage}");
            f.prepare(&id, "one").await;
            let gate = Arc::new(ShutdownTestGate::default());
            if stage == "ims-poll" {
                *f.service.shutdown_hooks.ims_poll.lock().unwrap() = Some(gate.clone());
            } else {
                *f.service.shutdown_hooks.before_finish.lock().unwrap() = Some(gate.clone());
            }
            let owner = f.service.store.send_owner();
            let service = f.service.clone();
            let request = f.request(&id, "one");
            let http =
                tokio::spawn(async move { super::super::supervise(service, request, false).await });
            let frame = f.frame().await;
            if stage != "ims-poll" {
                f.respond(&frame, 200).await;
            }
            gate.entered.notified().await;
            assert_eq!(f.record(&id).await.state, "submitting");
            owner.begin_shutdown();
            http.abort();
            let _ = http.await;
            assert_eq!(
                owner.snapshot().0,
                1,
                "task gap lost bounded root admission"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(25), owner.finish_pending())
                    .await
                    .is_err()
            );
            let refused = super::super::supervise(
                f.service.clone(),
                f.request("after-close", "blocked"),
                true,
            )
            .await;
            assert_eq!(failure(refused), "backend_stopping");
            if stage == "archive-failure" {
                fs::create_dir(f.root.join("sms-archive.json")).unwrap();
            }
            if stage == "ims-poll" {
                f.respond(&frame, 200).await;
            }
            gate.release.notify_one();
            tokio::time::timeout(Duration::from_secs(3), owner.finish_pending())
                .await
                .unwrap()
                .unwrap();
            let record = f.record(&id).await;
            assert_eq!(record.state, "accepted");
            assert_eq!(record.archive_confirmed, stage != "archive-failure");
            assert_eq!(owner.snapshot(), (0, true, false));
            assert!(
                f.wire.frames.is_empty(),
                "shutdown replayed physical MESSAGE"
            );
            f.finish().await;
        }
    }
}
