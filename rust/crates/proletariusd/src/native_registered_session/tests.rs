use super::*;
use crate::{
    ims_sms_transactions::{NativeReportOwner, Service as Transactions},
    native_ims_calls::{CallBinding, LocalMedia, Media, MediaFuture},
    native_sms_consumer::Consumer,
    native_sms_originating::tests::{RegisteredFixture, registered_fixture},
    native_sms_originating::{Phase, SessionConfig},
    native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey},
};
use proletarius_sip::{
    Header, Headers, Message, MessageData, Method, Parser, Request, Response, Uri,
};
use proletarius_vowifi::{
    TunnelConfiguration,
    netstack_runtime::TunnelNetwork,
    netstack_udp::{PacketSocket, UdpSocketRegistry},
    registration_coordinator::SipConnectionId,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{net::UdpSocket, sync::Notify, task::JoinHandle};
use zeroize::Zeroizing;
const BATCH: &str = "merged-dispatcher-sms-batch-0001";
const BUDGET: Duration = Duration::from_secs(5);
static NEXT: AtomicU64 = AtomicU64::new(0);
fn session_generation(n: u64) -> SessionGeneration {
    SessionGeneration::from_value(n).unwrap()
}

struct MediaNetwork {
    owner: Arc<native_ims_calls::media::Owner>,
    peer: PacketSocket,
    stop: watch::Sender<bool>,
    local: JoinHandle<()>,
    remote: JoinHandle<()>,
}
impl MediaNetwork {
    async fn new(generation: SessionGeneration) -> Self {
        let config = |ip: &str| TunnelConfiguration {
            ipv4_addresses: vec![ip.parse().unwrap()],
            ..Default::default()
        };
        let a = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
        let b = TunnelNetwork::new(&config("10.0.0.3")).unwrap();
        let (ab, ab_rx) = mpsc::channel(256);
        let (ba, ba_rx) = mpsc::channel(256);
        let (stop, stopped) = watch::channel(false);
        let (wa, ra) = watch::channel(0);
        let (wb, rb) = watch::channel(0);
        let (ca, sa) = UdpSocketRegistry::new(wa);
        let (cb, sb) = UdpSocketRegistry::new(wb);
        let remote_stop = stopped.clone();
        let local = tokio::spawn(async move {
            a.run_managed(
                smoltcp::iface::SocketSet::new(vec![]),
                ba_rx,
                ab,
                stopped,
                ra,
                sa,
            )
            .await
            .unwrap();
        });
        let remote = tokio::spawn(async move {
            b.run_managed(
                smoltcp::iface::SocketSet::new(vec![]),
                ab_rx,
                ba,
                remote_stop,
                rb,
                sb,
            )
            .await
            .unwrap();
        });
        let peer = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
        Self {
            owner: native_ims_calls::media::Owner::new(generation, "10.0.0.2".parse().unwrap(), ca),
            peer,
            stop,
            local,
            remote,
        }
    }
    async fn finish(mut self) {
        self.peer.close().await.unwrap();
        self.stop.send_replace(true);
        self.local.await.unwrap();
        self.remote.await.unwrap();
    }
}
/// The gate holds an actual native media worker open. Release still calls the
/// production stop implementation and waits for UDP/PCM/codec completion.
struct HeldMedia {
    owner: Arc<native_ims_calls::media::Owner>,
    hold: AtomicBool,
    stopping: Arc<Notify>,
    release: Arc<Notify>,
}
impl Media for HeldMedia {
    fn hold_transmit(self: Arc<Self>, b: CallBinding, hold: bool) -> MediaFuture<()> {
        self.owner.clone().hold_transmit(b, hold)
    }
    fn prepare(
        self: Arc<Self>,
        b: CallBinding,
        r: proletarius_call::sdp::AudioNegotiation,
    ) -> MediaFuture<()> {
        self.owner.clone().prepare(b, r)
    }
    fn discard(self: Arc<Self>, b: CallBinding) -> MediaFuture<()> {
        self.owner.clone().discard(b)
    }
    fn terminal_failure(&self, b: &CallBinding) -> Option<native_ims_calls::Failure> {
        self.owner.terminal_failure(b)
    }
    fn statistics(&self, b: &CallBinding) -> native_ims_calls::media::Stats {
        self.owner.statistics(b)
    }
    fn allocate(self: Arc<Self>, b: CallBinding) -> MediaFuture<LocalMedia> {
        self.owner.clone().allocate(b)
    }
    fn configure(
        self: Arc<Self>,
        b: CallBinding,
        r: proletarius_call::sdp::AudioNegotiation,
    ) -> MediaFuture<()> {
        self.owner.clone().configure(b, r)
    }
    fn start(self: Arc<Self>, b: CallBinding, early: bool) -> MediaFuture<()> {
        self.owner.clone().start(b, early)
    }
    fn dtmf(self: Arc<Self>, b: CallBinding, digit: char) -> MediaFuture<()> {
        self.owner.clone().dtmf(b, digit)
    }
    fn stop(self: Arc<Self>, b: CallBinding) -> MediaFuture<()> {
        Box::pin(async move {
            self.stopping.notify_one();
            if self.hold.load(Ordering::SeqCst) {
                self.release.notified().await;
            }
            self.owner.clone().stop(b).await
        })
    }
}
struct Harness {
    _supervisor: NativeVoWiFiSupervisor,
    generation: SessionGeneration,
    stop: watch::Sender<bool>,
    _lease: watch::Sender<bool>,
    controls: mpsc::Sender<Control>,
    injected: mpsc::Sender<Event>,
    notices: mpsc::Receiver<Notice>,
    notice_sender: mpsc::Sender<Notice>,
    commands: mpsc::Sender<native_ims_actor::Command>,
    actor: JoinHandle<native_ims_actor::Exit>,
    dispatcher: Option<JoinHandle<Exit>>,
    registered: RegisteredFixture,
    network: MediaNetwork,
    media: Arc<HeldMedia>,
    archive: PathBuf,
}
impl Harness {
    async fn new() -> Self {
        Self::new_with_registered_hook(|_| {}).await
    }

    async fn new_with_registered_hook(
        registered_hook: impl FnOnce(
            &proletarius_vowifi::registration_coordinator::RegisteredImsSession,
        ),
    ) -> Self {
        Self::new_with_options(
            registered_hook,
            crate::ims_sms_transactions::NativeSmsArchive::Ordinary,
        )
        .await
    }

    async fn new_with_sms_archive(archive: crate::ims_sms_transactions::NativeSmsArchive) -> Self {
        Self::new_with_options(|_| {}, archive).await
    }

    async fn new_with_options(
        registered_hook: impl FnOnce(
            &proletarius_vowifi::registration_coordinator::RegisteredImsSession,
        ),
        sms_archive: crate::ims_sms_transactions::NativeSmsArchive,
    ) -> Self {
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let key = ReaderSessionKey::new("combined-reader", "8901234567890123456").unwrap();
        let permit = supervisor.reserve(key.clone()).unwrap();
        supervisor.mark_running(&permit).unwrap();
        let generation = permit.generation();
        let stop = permit.cancellation_sender();
        let mut registered = registered_fixture(permit.stop_signal()).await;
        let incoming_snapshot = registered
            .session
            .as_ref()
            .unwrap()
            .message_context("sip:configured@ims.example", &[])
            .unwrap();
        let network = MediaNetwork::new(generation).await;
        let media = Arc::new(HeldMedia {
            owner: network.owner.clone(),
            hold: AtomicBool::new(false),
            stopping: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        });
        let (commands, input, injected, events) = native_ims_actor::channels();
        let (lease, lost) = watch::channel(false);
        let id = NEXT.fetch_add(1, Ordering::SeqCst);
        let archive = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../build/tmp/iteration-483-dispatcher")
            .join(format!("{}-{id}", std::process::id()));
        std::fs::create_dir_all(&archive).unwrap();
        let transactions = Arc::new(
            NativeReportOwner::new(
                Transactions::default(),
                format!("combined-transaction-session-{id:04}"),
            )
            .unwrap(),
        );
        let outgoing = Originating::new(
            transactions.clone(),
            SessionConfig {
                generation,
                snapshot: &registered.snapshot,
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
                archive.clone(),
                format!("sub1_{}", "a".repeat(64)),
                generation,
                commands.clone(),
                permit.stop_signal(),
                lost.clone(),
                transactions,
            )
            .with_archive(sms_archive),
            incoming_snapshot,
            None,
            74,
        );
        let calls = native_ims_calls::Service::new(
            generation,
            native_ims_calls::Config {
                local_ip: registered.snapshot.local_ip(),
                port_c: registered.snapshot.local_port(),
                contact: "<sip:authorized@10.0.0.2:5064>".into(),
                from_uri: registered.snapshot.routing().identity().into(),
                preferred_identity: registered.snapshot.routing().identity().into(),
                route: registered.snapshot.routing().routes().join(", "),
                service_routes: registered.snapshot.routing().routes().to_vec(),
                encoded_icsi: String::new(),
                phone_context: String::new(),
                home_domain: "ims.example".into(),
                realm: "ims.example".into(),
                user_agent: "combined-native-test".into(),
                security_verify: String::new(),
                pani: String::new(),
                early_media_header: false,
                evs_only: false,
                sms_only: false,
                ussd_service_selector: true,
            },
            commands.clone(),
            media.clone(),
        )
        .unwrap();
        let consumers = Consumers::new(incoming, outgoing, calls).unwrap();
        registered_hook(registered.session.as_ref().unwrap());
        let session = registered.session.take().unwrap();
        let actor_events = injected.clone();
        let actor_lease = lost.clone();
        let actor = tokio::spawn(async move {
            native_ims_actor::run(
                proletarius_vowifi::sip_session_runtime::RegisteredSipRuntime::new(session),
                &key,
                permit,
                actor_lease,
                input,
                actor_events,
            )
            .await
        });
        let (controls, control_input) = controls();
        let (notice_sender, notices) = mpsc::channel(1);
        let dispatcher = tokio::spawn(consumers.run(
            events,
            commands.clone(),
            control_input,
            notice_sender.clone(),
        ));
        Self {
            _supervisor: supervisor,
            generation,
            stop,
            _lease: lease,
            controls,
            injected,
            notices,
            notice_sender,
            commands,
            actor,
            dispatcher: Some(dispatcher),
            registered,
            network,
            media,
            archive,
        }
    }
    async fn frame(&mut self) -> Message {
        let wire = tokio::time::timeout(BUDGET, self.registered.frames.recv())
            .await
            .unwrap()
            .unwrap();
        let (m, used) = Parser::default().parse(wire.as_slice(), false).unwrap();
        assert_eq!(used, wire.len());
        m
    }
    async fn request(&mut self, method: Method) -> Request {
        let Message::Request(r) = self.frame().await else {
            panic!("request expected")
        };
        assert_eq!(r.method, method);
        r
    }
    async fn peer(&self, message: Message) {
        self.registered
            .replies
            .as_ref()
            .unwrap()
            .send(Zeroizing::new(message.encode()))
            .await
            .unwrap();
    }
    async fn call(
        &self,
        generation: SessionGeneration,
        command: CallCommand,
    ) -> Result<native_ims_calls::Snapshot, Failure> {
        let (reply, result) = oneshot::channel();
        self.controls
            .send(Control::Call {
                generation,
                command,
                reply,
            })
            .await
            .unwrap();
        tokio::time::timeout(BUDGET, result).await.unwrap().unwrap()
    }
    async fn ussd(
        &self,
        generation: SessionGeneration,
        command: UssdCommand,
    ) -> Result<UssdReply, Failure> {
        let (reply, result) = oneshot::channel();
        self.controls
            .send(Control::Ussd {
                generation,
                command,
                reply,
            })
            .await
            .unwrap();
        tokio::time::timeout(BUDGET, result).await.unwrap().unwrap()
    }
    async fn sms(&self, generation: SessionGeneration, text: &str) -> Result<Receipt, Failure> {
        let (reply, result) = oneshot::channel();
        self.controls
            .send(Control::SendText {
                input: TextSend {
                    generation,
                    batch_id: BATCH.into(),
                    recipient: Zeroizing::new("+447700900123".into()),
                    text: Zeroizing::new(text.into()),
                    request_status_report: true,
                },
                reply,
            })
            .await
            .unwrap();
        tokio::time::timeout(BUDGET, result).await.unwrap().unwrap()
    }
    async fn sms_status(&self, generation: SessionGeneration) -> Result<Option<Receipt>, Failure> {
        let (reply, result) = oneshot::channel();
        self.controls
            .send(Control::SmsStatus {
                generation,
                batch_id: BATCH.into(),
                reply,
            })
            .await
            .unwrap();
        tokio::time::timeout(BUDGET, result).await.unwrap().unwrap()
    }
    async fn sms_cancel(&self, generation: SessionGeneration) -> Result<Receipt, Failure> {
        let (reply, result) = oneshot::channel();
        self.controls
            .send(Control::SmsCancel {
                generation,
                batch_id: BATCH.into(),
                reply,
            })
            .await
            .unwrap();
        tokio::time::timeout(BUDGET, result).await.unwrap().unwrap()
    }
    async fn state(&self, expected: &str) -> native_ims_calls::Snapshot {
        tokio::time::timeout(BUDGET, async {
            loop {
                let snapshot = self
                    .call(self.generation, CallCommand::Snapshot)
                    .await
                    .unwrap();
                if snapshot.state == expected {
                    return snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    async fn established_call(&mut self) -> native_ims_calls::Snapshot {
        self.call(self.generation, CallCommand::Dial("+447700900456".into()))
            .await
            .unwrap();
        let invite = self.request(Method::INVITE).await;
        assert!(
            invite
                .data
                .headers
                .get("From")
                .unwrap()
                .value()
                .contains("sip:authorized@ims.example")
        );
        let remote = self.network.peer.local_addr();
        let sdp = proletarius_call::sdp::build_local_sdp(
            7,
            remote.ip(),
            remote.port(),
            false,
            None,
            None,
            None,
            false,
        )
        .unwrap();
        self.peer(Message::Response(response(
            &invite,
            200,
            sdp.into_bytes(),
            Some("application/sdp"),
        )))
        .await;
        self.request(Method::ACK).await;
        let snapshot = self.state("active").await;
        assert!(snapshot.media_ready);
        snapshot
    }
    async fn cleanup(self, exit: &Exit) {
        assert!(exit.media_cleanup.is_ok());
        self.stop.send_replace(true);
        drop(self.commands);
        drop(self.injected);
        drop(self.controls);
        let actor = tokio::time::timeout(BUDGET, self.actor)
            .await
            .unwrap()
            .unwrap();
        assert!(actor.cleanup.is_ok());
        self.registered.finish().await;
        self.network.finish().await;
    }
    async fn finish(mut self) -> Exit {
        self.stop.send_replace(true);
        let exit = tokio::time::timeout(BUDGET, self.dispatcher.take().unwrap())
            .await
            .unwrap()
            .unwrap();
        self.cleanup(&exit).await;
        exit
    }
}
// Responses preserve the exact actor transaction identity, remote dialog tag,
// and registered request routing; the synthetic peer never opens a host socket.
fn response(request: &Request, status: u16, body: Vec<u8>, mime: Option<&str>) -> Response {
    let mut headers = Headers::default();
    for name in ["Via", "From", "Call-ID", "CSeq"] {
        headers.push(request.data.headers.get(name).unwrap().clone());
    }
    headers.push(
        Header::new(
            "To",
            format!(
                "{};tag=network-tag",
                request.data.headers.get("To").unwrap().value()
            ),
        )
        .unwrap(),
    );
    headers.push(Header::new("Contact", "<sip:network@ims.example>").unwrap());
    if let Some(mime) = mime {
        headers.push(Header::new("Content-Type", mime).unwrap());
    }
    Response {
        status,
        reason: "fixture".into(),
        data: MessageData::new("SIP/2.0", headers, body),
    }
}
fn ussd_info(invite: &Request, seq: u32, text: &str) -> Request {
    let mut headers = Headers::default();
    for (name, value) in [
        ("Via", "SIP/2.0/TCP network;branch=z9hG4bKinfo".into()),
        (
            "From",
            format!(
                "{};tag=network-tag",
                invite.data.headers.get("To").unwrap().value()
            ),
        ),
        (
            "To",
            invite.data.headers.get("From").unwrap().value().to_owned(),
        ),
        (
            "Call-ID",
            invite
                .data
                .headers
                .get("Call-ID")
                .unwrap()
                .value()
                .to_owned(),
        ),
        ("CSeq", format!("{seq} INFO")),
        ("Info-Package", "g.3gpp.ussd".into()),
        ("Content-Type", "application/vnd.3gpp.ussd+xml".into()),
    ] {
        headers.push(Header::new(name, value).unwrap());
    }
    Request {
        method: Method::INFO,
        uri: Uri::parse("sip:authorized@ims.example").unwrap(),
        data: MessageData::new(
            "SIP/2.0",
            headers,
            proletarius_ims::ussd_xml::encode(text).unwrap(),
        ),
    }
}
fn event(generation: SessionGeneration, event: RegisteredSipEvent) -> Event {
    Event {
        generation,
        reply_state: IncomingReply::Unanswered,
        event,
    }
}

#[tokio::test]
async fn registered_sms_call_and_ussd_share_one_actor_without_waiting_for_pending_results() {
    let mut h = Harness::new().await;
    h.sms(h.generation, &"A".repeat(161)).await.unwrap();
    let first = h.request(Method::MESSAGE).await;
    assert!(
        first
            .data
            .headers
            .get("From")
            .unwrap()
            .value()
            .contains("sip:authorized@ims.example")
    );
    assert!(
        first
            .data
            .headers
            .get("Route")
            .unwrap()
            .value()
            .contains("registered-route")
    );
    let UssdReply::Pending(mut ussd) = h
        .ussd(h.generation, UssdCommand::Start("*100#".into()))
        .await
        .unwrap()
    else {
        panic!("pending")
    };
    let invite = h.request(Method::INVITE).await;
    assert!(
        invite
            .data
            .headers
            .get("Accept-Contact")
            .unwrap()
            .value()
            .contains("g.3gpp.ussd")
    );
    assert!(matches!(
        ussd.receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    // A waiting USSD result cannot monopolize the dispatcher or block voice.
    let voice = h.established_call().await;
    assert_ne!(voice.call_id, ussd.session_id);
    h.peer(Message::Response(response(&first, 202, vec![], None)))
        .await;
    let second = h.request(Method::MESSAGE).await;
    assert_ne!(
        first.data.headers.get("Call-ID"),
        second.data.headers.get("Call-ID")
    );
    assert!(
        second
            .data
            .headers
            .get("CSeq")
            .unwrap()
            .value()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<u32>()
            .unwrap()
            > first
                .data
                .headers
                .get("CSeq")
                .unwrap()
                .value()
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u32>()
                .unwrap()
    );
    h.peer(Message::Response(response(&second, 200, vec![], None)))
        .await;
    h.peer(Message::Request(ussd_info(&invite, 2, "Balance?")))
        .await;
    h.peer(Message::Response(response(&invite, 200, vec![], None)))
        .await;
    h.request(Method::ACK).await;
    let Message::Response(reply) = h.frame().await else {
        panic!("INFO response")
    };
    assert_eq!(reply.status, 200);
    let prompt = tokio::time::timeout(BUDGET, ussd.receiver)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(prompt.text, "Balance?");
    assert!(prompt.needs_input);
    let UssdReply::Pending(continuation) = h
        .ussd(
            h.generation,
            UssdCommand::Continue {
                session_id: prompt.session_id.clone(),
                input: "1".into(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("pending continuation")
    };
    let input = h.request(Method::INFO).await;
    assert_eq!(
        input.data.headers.get("Info-Package").unwrap().value(),
        "g.3gpp.ussd"
    );
    h.peer(Message::Response(response(&input, 200, vec![], None)))
        .await;
    h.peer(Message::Request(ussd_info(&invite, 3, "Done")))
        .await;
    let Message::Response(reply) = h.frame().await else {
        panic!("INFO response")
    };
    assert_eq!(reply.status, 200);
    let result = tokio::time::timeout(BUDGET, continuation.receiver)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.text, "Done");
    let UssdReply::Complete(cancelled) = h
        .ussd(
            h.generation,
            UssdCommand::Cancel {
                session_id: prompt.session_id,
            },
        )
        .await
        .unwrap()
    else {
        panic!("cancel complete")
    };
    assert!(cancelled.complete);
    let ussd_bye = h.request(Method::BYE).await;
    assert_eq!(
        ussd_bye.data.headers.get("Call-ID").unwrap().value(),
        cancelled.session_id
    );
    let receipt = h.sms_status(h.generation).await.unwrap().unwrap();
    assert_eq!(receipt.phase, Phase::Accepted);
    assert_eq!(receipt.accepted_parts, 2);
    assert_eq!(
        h.sms_cancel(h.generation).await.unwrap().phase,
        Phase::Accepted
    );
    let live = h.call(h.generation, CallCommand::Snapshot).await.unwrap();
    assert_eq!(live.state, "active");
    assert_eq!(live.call_id, voice.call_id);
    // An independent network RP-DATA uses the same actor, is committed before
    // SIP 200, and allocates its RP-ACK CSeq from the shared transaction owner.
    let tpdu = [
        0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x41,
    ];
    let mut rpdu = vec![1, 42, 0, 0, tpdu.len() as u8];
    rpdu.extend_from_slice(&tpdu);
    let mut headers = Headers::default();
    for (name, value) in [
        ("Via", "SIP/2.0/TCP network;branch=z9hG4bKinbound"),
        ("From", "<sip:smsc@ims.example>"),
        ("P-Asserted-Identity", "<sip:smsc@ims.example>"),
        ("To", "<sip:authorized@ims.example>"),
        ("Call-ID", "combined-incoming-sms"),
        ("CSeq", "73 MESSAGE"),
        ("Content-Type", "application/vnd.3gpp.sms"),
    ] {
        headers.push(Header::new(name, value).unwrap());
    }
    h.peer(Message::Request(Request {
        method: Method::MESSAGE,
        uri: Uri::parse("sip:authorized@ims.example").unwrap(),
        data: MessageData::new("SIP/2.0", headers, rpdu),
    }))
    .await;
    let Message::Response(received) = h.frame().await else {
        panic!("MT SIP response")
    };
    assert_eq!(received.status, 200);
    assert_eq!(
        proletarius_store::ArchiveStore::new(h.archive.join("sms-archive.json"))
            .load()
            .unwrap()
            .len(),
        1
    );
    let report = h.request(Method::MESSAGE).await;
    assert_eq!(report.data.body, proletarius_sms::build_ack(42));
    assert!(
        report
            .data
            .headers
            .get("CSeq")
            .unwrap()
            .value()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<u32>()
            .unwrap()
            > second
                .data
                .headers
                .get("CSeq")
                .unwrap()
                .value()
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u32>()
                .unwrap()
    );
    assert!(matches!(
        tokio::time::timeout(BUDGET, h.notices.recv())
            .await
            .unwrap()
            .unwrap(),
        Notice::Sms(SessionOutcome::ReportSubmitted(_))
    ));
    h.peer(Message::Response(response(&report, 200, vec![], None)))
        .await;
    assert!(matches!(
        tokio::time::timeout(BUDGET, h.notices.recv())
            .await
            .unwrap()
            .unwrap(),
        Notice::Sms(SessionOutcome::ReportFinished { .. })
    ));
    // Real Swift PCM bridge drives the production RTP worker while SMS/USSD
    // final responses have passed through the same actor event loop.
    let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut pcm = format!("{}\0", live.audio_token).into_bytes();
    pcm.extend_from_slice(&[0; 640]);
    host.send_to(&pcm, ("127.0.0.1", live.audio_port))
        .await
        .unwrap();
    let packet = tokio::time::timeout(BUDGET, h.network.peer.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !proletarius_call::RtpPacket::parse(&packet.bytes)
            .unwrap()
            .payload
            .is_empty()
    );
    let exit = h.finish().await;
    assert_eq!(exit.reason, StopReason::Requested);
    assert_eq!(exit.sms[0].phase, Phase::Accepted);
}

#[tokio::test]
async fn stale_generation_and_abandoned_unaccepted_controls_have_no_sip_or_media_io() {
    let mut h = Harness::new().await;
    let stale_generation = session_generation(h.generation.value() + 1);
    assert_eq!(
        h.sms(stale_generation, "BAL").await.unwrap_err().code,
        "native_session_stale_generation"
    );
    for command in [
        CallCommand::Dial("+447700900456".into()),
        CallCommand::Answer,
        CallCommand::Reject,
        CallCommand::Hangup,
        CallCommand::Dtmf('1'),
        CallCommand::Snapshot,
    ] {
        assert_eq!(
            h.call(stale_generation, command).await.err().unwrap().code,
            "native_session_stale_generation"
        );
    }
    for command in [
        UssdCommand::Start("*100#".into()),
        UssdCommand::Continue {
            session_id: "stale".into(),
            input: "1".into(),
        },
        UssdCommand::Cancel {
            session_id: "stale".into(),
        },
    ] {
        assert_eq!(
            h.ussd(stale_generation, command).await.err().unwrap().code,
            "native_session_stale_generation"
        );
    }
    assert_eq!(
        h.sms_status(stale_generation).await.unwrap_err().code,
        "native_session_stale_generation"
    );
    assert_eq!(
        h.sms_cancel(stale_generation).await.unwrap_err().code,
        "native_session_stale_generation"
    );
    h.injected
        .send(event(
            stale_generation,
            RegisteredSipEvent::RegistrationRenewalDue,
        ))
        .await
        .unwrap();
    for method in [Method::MESSAGE, Method::INVITE, Method::INFO] {
        let mut headers = Headers::default();
        for (name, value) in [
            ("Via", "SIP/2.0/TCP retired;branch=z9hG4bKretired"),
            ("From", "<sip:smsc@ims.example>;tag=old"),
            ("To", "<sip:authorized@ims.example>"),
            ("Call-ID", "retired-dialog"),
            ("Content-Type", "application/vnd.3gpp.sms"),
        ] {
            headers.push(Header::new(name, value).unwrap());
        }
        headers.push(Header::new("CSeq", format!("1 {}", method.as_str())).unwrap());
        h.injected
            .send(event(
                stale_generation,
                RegisteredSipEvent::Message {
                    connection: SipConnectionId::Primary,
                    response_ticket: None,
                    message: Box::new(RegisteredSipMessage::Request {
                        method,
                        uri: Uri::parse("sip:authorized@ims.example").unwrap(),
                        version: "SIP/2.0".into(),
                        headers,
                        body: Zeroizing::new(vec![1, 42, 0, 0, 1, 0]),
                    }),
                },
            ))
            .await
            .unwrap();
    }
    let (reply, result) = oneshot::channel();
    drop(result);
    h.controls
        .send(Control::Ussd {
            generation: h.generation,
            command: UssdCommand::Start("*100#".into()),
            reply,
        })
        .await
        .unwrap();
    let (reply, result) = oneshot::channel();
    drop(result);
    h.controls
        .send(Control::Call {
            generation: h.generation,
            command: CallCommand::Dial("+447700900456".into()),
            reply,
        })
        .await
        .unwrap();
    assert_eq!(
        h.call(h.generation, CallCommand::Snapshot)
            .await
            .unwrap()
            .state,
        "idle"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), h.registered.frames.recv())
            .await
            .is_err()
    );
    assert!(h.notices.try_recv().is_err());
    assert!(h.sms_status(h.generation).await.unwrap().is_none());
    assert!(
        !h.archive.join("sms-archive.json").exists(),
        "stale reader generation created SMS history"
    );
    let exit = h.finish().await;
    assert!(exit.sms.is_empty());
    assert!(exit.timer_failures.is_empty());
}

#[tokio::test]
async fn renewal_waits_for_live_call_and_allows_existing_hangup_without_a_new_session() {
    renewal_preserves_live_call_with_hooks(|_| {}, |_| {}, || true).await;
}

/// The ordinary reporter reuses this actual REGISTER/voice/media owner rather
/// than replacing its retirement proof with a copied lifecycle fixture.
pub(crate) async fn renewal_preserves_live_call_with_hooks(
    registered_hook: impl FnOnce(&proletarius_vowifi::registration_coordinator::RegisteredImsSession),
    renewal_hook: impl Fn(&Notice),
    ready_check: impl Fn() -> bool,
) {
    let mut h = Harness::new_with_registered_hook(registered_hook).await;
    let voice = h.established_call().await;
    h.injected
        .send(event(
            h.generation,
            RegisteredSipEvent::RegistrationRenewalDue,
        ))
        .await
        .unwrap();
    let renewal = tokio::time::timeout(BUDGET, h.notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        &renewal,
        Notice::Sms(SessionOutcome::RegistrationRenewalDue {
            outstanding_reports: 0
        })
    ));
    renewal_hook(&renewal);
    assert!(!h.dispatcher.as_ref().unwrap().is_finished());
    assert_eq!(
        h.call(h.generation, CallCommand::Snapshot)
            .await
            .unwrap()
            .call_id,
        voice.call_id
    );
    assert_eq!(
        h.call(h.generation, CallCommand::Dial("+447700900789".into()))
            .await
            .err()
            .unwrap()
            .code,
        "native_session_renewing"
    );
    assert_eq!(
        h.sms(h.generation, "BAL").await.unwrap_err().code,
        "native_session_renewing"
    );
    assert_eq!(
        h.ussd(h.generation, UssdCommand::Start("*100#".into()))
            .await
            .err()
            .unwrap()
            .code,
        "native_session_renewing"
    );
    assert!(
        ready_check(),
        "the ordinary API must retain the same ready owner during renewal until the live call drains"
    );
    h.call(h.generation, CallCommand::Hangup).await.unwrap();
    let bye = h.request(Method::BYE).await;
    assert_eq!(
        bye.data.headers.get("Call-ID").unwrap().value(),
        voice.call_id
    );
    let exit = tokio::time::timeout(BUDGET, h.dispatcher.take().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit.reason, StopReason::RenewalDue);
    assert!(exit.media_cleanup.is_ok());
    assert!(exit.sms.is_empty());
    h.cleanup(&exit).await;
}

#[tokio::test]
async fn backpressured_notice_preserves_sms_deadline_and_cancel_status_controls() {
    let mut h = Harness::new().await;
    h.notice_sender
        .send(Notice::Failed("observer-marker".into()))
        .await
        .unwrap();
    h.sms(h.generation, "BAL").await.unwrap();
    h.request(Method::MESSAGE).await;
    let mut headers = Headers::default();
    headers.push(Header::new("Call-ID", "unowned-notice").unwrap());
    headers.push(Header::new("CSeq", "1 OPTIONS").unwrap());
    h.injected
        .send(event(
            h.generation,
            RegisteredSipEvent::Message {
                connection: SipConnectionId::Primary,
                response_ticket: None,
                message: Box::new(RegisteredSipMessage::Response {
                    status: 180,
                    reason: "unowned".into(),
                    version: "SIP/2.0".into(),
                    headers,
                    body: Zeroizing::new(vec![]),
                }),
            },
        ))
        .await
        .unwrap();
    // Keep the observer full through the actual production 15s SMS deadline.
    // The pending notice must not stop timer advancement or typed controls.
    tokio::time::timeout(Duration::from_secs(18), async {
        loop {
            if h.sms_status(h.generation).await.unwrap().unwrap().phase == Phase::OutcomeUnknown {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        h.sms_cancel(h.generation).await.unwrap().phase,
        Phase::OutcomeUnknown
    );
    assert!(
        h.registered.frames.try_recv().is_err(),
        "timeout/cancel must not replay the SMS"
    );
    let exit = h.finish().await;
    assert_eq!(exit.sms[0].phase, Phase::OutcomeUnknown);
    assert!(matches!(
        exit.undelivered_notice,
        Some(Notice::Unhandled(_))
    ));
    assert!(exit.timer_failures.is_empty());
}

#[tokio::test]
async fn stopping_freezes_sms_and_waits_for_actual_media_worker_and_socket_retirement() {
    let mut h = Harness::new().await;
    h.sms(h.generation, "BAL").await.unwrap();
    h.request(Method::MESSAGE).await;
    let UssdReply::Pending(ussd) = h
        .ussd(h.generation, UssdCommand::Start("*100#".into()))
        .await
        .unwrap()
    else {
        panic!("pending USSD")
    };
    h.request(Method::INVITE).await;
    let voice = h.established_call().await;
    let binding = CallBinding {
        generation: h.generation,
        call_id: voice.call_id.clone(),
        invite_cseq: 1,
    };
    let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut pcm = format!("{}\0", voice.audio_token).into_bytes();
    pcm.extend_from_slice(&[0; 640]);
    host.send_to(&pcm, ("127.0.0.1", voice.audio_port))
        .await
        .unwrap();
    let packet = tokio::time::timeout(BUDGET, h.network.peer.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !proletarius_call::RtpPacket::parse(&packet.bytes)
            .unwrap()
            .payload
            .is_empty()
    );
    h.media.hold.store(true, Ordering::SeqCst);
    h.stop.send_replace(true);
    tokio::time::timeout(BUDGET, h.media.stopping.notified())
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(BUDGET, ussd.receiver)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap()
            .code,
        "ims_ussd_session_stopped"
    );
    assert!(
        !h.dispatcher.as_ref().unwrap().is_finished(),
        "observer exit cannot fake media completion"
    );
    assert!(
        UdpSocket::bind(("127.0.0.1", voice.audio_port))
            .await
            .is_err(),
        "actual PCM socket was released before media stop"
    );
    h.media.hold.store(false, Ordering::SeqCst);
    h.media.release.notify_one();
    let exit = tokio::time::timeout(BUDGET, h.dispatcher.take().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit.reason, StopReason::Requested);
    assert!(exit.media_cleanup.is_ok());
    assert_eq!(exit.sms[0].phase, Phase::OutcomeUnknown);
    assert_eq!(exit.sms[0].accepted_parts, 0);
    assert!(
        h.network.owner.clone().start(binding, false).await.is_err(),
        "retired worker still accepted media commands"
    );
    let rebound = UdpSocket::bind(("127.0.0.1", voice.audio_port))
        .await
        .unwrap();
    drop(rebound);
    h.cleanup(&exit).await;
}

#[tokio::test]
async fn profile_switch_atomically_refuses_active_call_and_closes_idle_dispatcher() {
    let mut h = Harness::new().await;
    let live = h.established_call().await;
    let (reply, response) = oneshot::channel();
    h.controls
        .send(Control::PrepareProfileSwitch {
            generation: h.generation,
            reply,
        })
        .await
        .unwrap();
    assert_eq!(
        response.await.unwrap().unwrap_err().code,
        "module_vowifi_call_active"
    );
    assert_eq!(
        h.call(h.generation, CallCommand::Snapshot)
            .await
            .unwrap()
            .call_id,
        live.call_id
    );
    assert!(!h.dispatcher.as_ref().unwrap().is_finished());
    h.call(h.generation, CallCommand::Hangup).await.unwrap();
    h.request(Method::BYE).await;
    h.state("ended").await;
    let (reply, response) = oneshot::channel();
    h.controls
        .send(Control::PrepareProfileSwitch {
            generation: h.generation,
            reply,
        })
        .await
        .unwrap();
    response.await.unwrap().unwrap();
    let exit = tokio::time::timeout(BUDGET, h.dispatcher.take().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit.reason, StopReason::Requested);
    assert!(h.controls.is_closed());
    assert!(exit.media_cleanup.is_ok());
    h.cleanup(&exit).await;
}

#[tokio::test]
async fn profile_switch_rejects_stale_generation_without_closing_live_owner() {
    let h = Harness::new().await;
    let (reply, response) = oneshot::channel();
    h.controls
        .send(Control::PrepareProfileSwitch {
            generation: session_generation(h.generation.value() + 1),
            reply,
        })
        .await
        .unwrap();
    assert_eq!(
        response.await.unwrap().unwrap_err().code,
        "native_session_stale_generation"
    );
    assert_eq!(
        h.call(h.generation, CallCommand::Snapshot)
            .await
            .unwrap()
            .state,
        "idle"
    );
    h.finish().await;
}

#[path = "status_report_tests.rs"]
mod status_report_tests;
