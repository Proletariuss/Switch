use super::*;
use crate::ims_sms_transactions::{NativeReportOwner, Service};
use proletarius_sip::{Header, Headers, Message, MessageData, Parser, Response};

const BATCH: &str = "native-originating-batch-0001";
fn generation(n: u64) -> SessionGeneration {
    SessionGeneration::from_value(n).unwrap()
}
struct Fixture {
    outgoing: Originating,
    commands: mpsc::Receiver<Command>,
    stop: watch::Sender<bool>,
    _lease: watch::Sender<bool>,
}
fn fixture(capacity: usize) -> Fixture {
    let owner = Arc::new(
        NativeReportOwner::new(Service::default(), "native-originating-session-0001".into())
            .unwrap(),
    );
    owner.bind_generation(generation(1)).unwrap();
    let (commands, input) = mpsc::channel(capacity);
    let (stop, stopped) = watch::channel(false);
    let (lease, lost) = watch::channel(false);
    Fixture {
        outgoing: Originating {
            owner,
            generation: generation(1),
            public_identity: Zeroizing::new("sip:registered@ims.example".into()),
            local_host: Zeroizing::new("10.0.0.2".into()),
            local_port: 5064,
            minimum_cseq: 74,
            routes: Zeroizing::new(vec!["<sip:registered-route@ims.example;lr>".into()]),
            security: Some(Zeroizing::new("ipsec-3gpp;alg=hmac-sha-1-96".into())),
            pani: Some(Zeroizing::new(
                "IEEE-802.11;utran-cell-id-3gpp=0010100000000000".into(),
            )),
            smsc: Zeroizing::new("+447802002606".into()),
            target: Zeroizing::new("sip:smsc@ims.example".into()),
            commands,
            stop_signal: stopped,
            lease_lost: lost,
            stopped: false,
            active: None,
            batches: HashMap::new(),
            recent: VecDeque::new(),
        },
        commands: input,
        stop,
        _lease: lease,
    }
}
fn input(text: &str) -> TextSend {
    TextSend {
        generation: generation(1),
        batch_id: BATCH.into(),
        recipient: Zeroizing::new("+447700900123".into()),
        text: Zeroizing::new(text.into()),
        request_status_report: true,
    }
}
fn response(
    request: &proletarius_sip::Request,
    status: u16,
    session_generation: SessionGeneration,
) -> Event {
    Event {
        generation: session_generation,
        reply_state: crate::native_ims_actor::IncomingReply::Unanswered,
        event: RegisteredSipEvent::Message {
            connection: SipConnectionId::Primary,
            response_ticket: None,
            message: Box::new(RegisteredSipMessage::Response {
                status,
                reason: "Test".into(),
                version: "SIP/2.0".into(),
                headers: request.data.headers.clone(),
                body: Zeroizing::new(vec![]),
            }),
        },
    }
}
fn report(request: &proletarius_sip::Request, reference: u8, error: Option<u8>) -> Event {
    let mut headers = Headers::default();
    headers.push(Header::new("Content-Type", "application/vnd.3gpp.sms").unwrap());
    headers.push(
        Header::new(
            "In-Reply-To",
            request.data.headers.get("Call-ID").unwrap().value(),
        )
        .unwrap(),
    );
    Event {
        generation: generation(1),
        reply_state: crate::native_ims_actor::IncomingReply::Unanswered,
        event: RegisteredSipEvent::Message {
            connection: SipConnectionId::Primary,
            response_ticket: None,
            message: Box::new(RegisteredSipMessage::Request {
                method: proletarius_sip::Method::MESSAGE,
                uri: proletarius_sip::Uri::parse("sip:registered@ims.example").unwrap(),
                version: "SIP/2.0".into(),
                headers,
                body: Zeroizing::new(match error {
                    None => vec![3, reference],
                    Some(c) => vec![5, reference, 1, c],
                }),
            }),
        },
    }
}
fn sent(
    f: &mut Fixture,
) -> (
    proletarius_sip::Request,
    oneshot::Sender<io::Result<ResponseTicket>>,
) {
    let Command::Send {
        generation: session_generation,
        request,
        reply,
        ..
    } = f.commands.try_recv().unwrap()
    else {
        panic!("typed Send");
    };
    assert_eq!(session_generation, generation(1));
    (request, reply)
}
#[test]
fn wrong_generation_and_duplicate_batch_never_submit_a_second_request() {
    let mut f = fixture(4);
    let mut wrong = input("BAL");
    wrong.generation = generation(2);
    assert_eq!(
        f.outgoing.start(wrong).unwrap_err(),
        "native_sms_session_mismatch"
    );
    assert!(f.commands.try_recv().is_err());
    assert!(f.outgoing.next_deadline().is_none());
    f.outgoing.start(input("BAL")).unwrap();
    let (_, write) = sent(&mut f);
    drop(write);
    f.outgoing.tick().unwrap();
    assert_eq!(
        f.outgoing.snapshot(BATCH).unwrap().phase,
        Phase::OutcomeUnknown
    );
    assert_eq!(
        f.outgoing.start(input("BAL")).unwrap_err(),
        "sms_batch_already_started"
    );
    assert!(f.commands.try_recv().is_err());
    assert!(f.outgoing.next_deadline().is_none());
}
#[test]
fn submitted_write_receipt_loss_remains_unknown_and_late_acceptance_never_sends_remaining_parts() {
    let mut f = fixture(4);
    let start = f.outgoing.start(input(&"A".repeat(161))).unwrap();
    assert_eq!(start.total_parts, 2);
    let (request, reply) = sent(&mut f);
    drop(reply);
    f.outgoing.tick().unwrap();
    assert!(f.outgoing.snapshot(BATCH).unwrap().outcome_unknown());
    assert!(
        !f.outgoing
            .process(&response(&request, 200, generation(2)))
            .unwrap()
    );
    let mut wrong = response(&request, 200, generation(1));
    if let RegisteredSipEvent::Message { connection, .. } = &mut wrong.event {
        *connection = SipConnectionId::Incoming(7);
    }
    assert!(!f.outgoing.process(&wrong).unwrap());
    let mut right = response(&request, 200, generation(1));
    if let RegisteredSipEvent::Message { message, .. } = &mut right.event
        && let RegisteredSipMessage::Response { headers, .. } = message.as_mut()
    {
        let via = headers.get("Via").unwrap().value().to_owned();
        headers.set(Header::new("Via", format!("{via};received=192.0.2.1;rport=5064")).unwrap());
        let sequence = request
            .data
            .headers
            .get("CSeq")
            .unwrap()
            .value()
            .split_ascii_whitespace()
            .next()
            .unwrap();
        headers.set(Header::new("CSeq", format!("{sequence}\tMESSAGE")).unwrap());
    }
    assert!(f.outgoing.process(&right).unwrap());
    let final_state = f.outgoing.snapshot(BATCH).unwrap();
    assert_eq!(final_state.accepted_parts, 1);
    assert_eq!(final_state.phase, Phase::Partial);
    assert!(final_state.outcome_unknown());
    assert!(!f.outgoing.process(&right).unwrap());
    assert!(f.commands.try_recv().is_err());
}
#[test]
fn stop_after_submission_keeps_unknown_and_fences_replacement_generation() {
    let mut f = fixture(4);
    f.outgoing.start(input("BAL")).unwrap();
    let (_request, _write) = sent(&mut f);
    f.stop.send_replace(true);
    f.outgoing.tick().unwrap();
    assert_eq!(
        f.outgoing.snapshot(BATCH).unwrap().phase,
        Phase::OutcomeUnknown
    );
    assert_eq!(
        f.outgoing.start(input("BAL")).unwrap_err(),
        "native_sms_session_stopped"
    );
    assert!(f.commands.try_recv().is_err());
}
#[test]
fn queue_full_timeout_aborts_only_unsubmitted_work() {
    let mut f = fixture(1);
    let (reply, _) = oneshot::channel();
    f.outgoing
        .commands
        .try_send(Command::Respond {
            generation: generation(1),
            connection: SipConnectionId::Primary,
            request: Box::new(RegisteredSipMessage::Response {
                status: 200,
                reason: "marker".into(),
                version: "SIP/2.0".into(),
                headers: Headers::default(),
                body: Zeroizing::new(vec![]),
            }),
            status: 200,
            reason: "marker".into(),
            reply,
        })
        .unwrap_or_else(|_| panic!("fill queue"));
    let state = f.outgoing.start(input("BAL")).unwrap();
    assert_eq!(state.phase, Phase::Prepared);
    f.outgoing.batches.get_mut(BATCH).unwrap().deadline =
        Some(Instant::now() - Duration::from_millis(1));
    f.outgoing.tick().unwrap();
    let state = f.outgoing.snapshot(BATCH).unwrap();
    assert_eq!(state.phase, Phase::Aborted);
    assert!(!state.outcome_unknown());
    assert_eq!(state.accepted_parts, 0);
    assert!(matches!(
        f.commands.try_recv().unwrap(),
        Command::Respond { .. }
    ));
    assert!(f.commands.try_recv().is_err());
}
#[test]
fn rp_result_uses_exact_reference_call_id_and_generation_without_reflecting_sms() {
    let mut f = fixture(4);
    f.outgoing.start(input("BAL")).unwrap();
    let (request, reply) = sent(&mut f);
    drop(reply);
    f.outgoing.tick().unwrap();
    let reference = request.data.body[1];
    let mut wrong = report(&request, reference, None);
    wrong.generation = generation(2);
    assert!(!f.outgoing.process(&wrong).unwrap());
    let mut wrong = report(&request, reference, None);
    if let RegisteredSipEvent::Message { message, .. } = &mut wrong.event
        && let RegisteredSipMessage::Request { headers, .. } = message.as_mut()
    {
        headers.set(Header::new("In-Reply-To", "another-call").unwrap());
    }
    assert!(!f.outgoing.process(&wrong).unwrap());
    let ack = report(&request, reference, None);
    assert!(f.outgoing.process(&ack).unwrap());
    assert!(f.outgoing.process(&ack).unwrap());
    assert_eq!(
        f.outgoing.snapshot(BATCH).unwrap().rp_results,
        vec![(1, RpResult::Ack)]
    );
    assert_eq!(
        f.outgoing.snapshot(BATCH).unwrap().phase,
        Phase::OutcomeUnknown
    );
    assert!(
        f.commands.try_recv().is_err(),
        "RP result must not become outgoing SMS or reflected RP-ACK"
    );
}
#[test]
fn independent_native_service_reuses_encoding_and_shares_report_cseq_without_json_adapter() {
    let service = Service::default();
    let owner =
        NativeReportOwner::new(service.clone(), "native-typed-session-0001".into()).unwrap();
    owner.bind_generation(generation(1)).unwrap();
    let routes = vec!["<sip:registered-route@ims.example;lr>".to_owned()];
    let context = NativeSmsRouting {
        generation: generation(1),
        minimum_cseq: 74,
        public_identity: "sip:registered@ims.example",
        local_host: "10.0.0.2",
        local_port: 5064,
        route_headers: &routes,
        security_verify: None,
        access_network_info: None,
    };
    let parts =
        prepare_native_text("+447700900123", "+447802002606", &"A".repeat(161), true).unwrap();
    assert_eq!(parts.len(), 2);
    let first = owner
        .prepare_part(BATCH, 2, "sip:smsc@ims.example", &context, &parts[0].rpdu)
        .unwrap();
    assert_eq!(first.part, 1);
    assert_eq!(first.total_parts, 2);
    assert_eq!(first.request.uri.to_string(), "sip:smsc@ims.example");
    assert_eq!(first.request.data.body, parts[0].rpdu.as_slice());
    assert_eq!(
        first.request.data.headers.get("Route").unwrap().value(),
        routes[0]
    );
    owner.submit_part(BATCH, &first.identity).unwrap();
    assert!(
        owner
            .prepare_part(BATCH, 2, "sip:smsc@ims.example", &context, &parts[1].rpdu)
            .is_err()
    );
    let observed = owner
        .observe_part(BATCH, &response(&first.request, 202, generation(1)).event)
        .unwrap();
    assert_eq!(
        observed.disposition,
        SmsResponseDisposition::PartAccepted { completed: false }
    );
    let report = service
        .prepare_delivery_report_wire(
            "native-typed-session-0001".into(),
            0,
            "sip:smsc@ims.example".into(),
            "incoming-call".into(),
            "sip:registered@ims.example".into(),
            "10.0.0.2".into(),
            5064,
            routes.clone(),
            None,
            None,
            9,
            Some(&owner),
        )
        .unwrap();
    let second = owner
        .prepare_part(BATCH, 2, "sip:smsc@ims.example", &context, &parts[1].rpdu)
        .unwrap();
    assert!(second.identity.cseq > report.cseq && report.cseq > first.identity.cseq);
    owner.submit_part(BATCH, &second.identity).unwrap();
    let observed = owner
        .observe_part(BATCH, &response(&second.request, 200, generation(1)).event)
        .unwrap();
    assert_eq!(
        observed.state.status,
        SmsSubmissionStatus::Accepted { parts: 2 }
    );
    assert!(
        owner
            .prepare_part(BATCH, 2, "sip:smsc@ims.example", &context, &parts[0].rpdu)
            .is_err()
    );
    assert_eq!(
        owner.bind_generation(generation(2)).unwrap_err(),
        "native_sms_session_mismatch"
    );
}

/// Real in-memory TCP + REGISTER. All packets stay between two smoltcp stacks;
/// no host network, card, IKE peer or internal HTTP adapter is used.
pub(crate) struct RegisteredFixture {
    pub(crate) session: Option<proletarius_vowifi::registration_coordinator::RegisteredImsSession>,
    pub(crate) snapshot: proletarius_vowifi::message_routing::RegisteredMessageContext,
    pub(crate) frames: mpsc::Receiver<Zeroizing<Vec<u8>>>,
    pub(crate) replies: Option<mpsc::Sender<Zeroizing<Vec<u8>>>>,
    network_stop: watch::Sender<bool>,
    peer: tokio::task::JoinHandle<()>,
    local: tokio::task::JoinHandle<()>,
    remote: tokio::task::JoinHandle<()>,
}
impl RegisteredFixture {
    pub(crate) async fn finish(mut self) {
        if let Some(session) = self.session.take() {
            session.shutdown().await.unwrap();
        }
        self.replies.take();
        self.peer.await.unwrap();
        self.network_stop.send_replace(true);
        self.local.await.unwrap();
        self.remote.await.unwrap();
    }
}
pub(crate) async fn registered_fixture(stop: watch::Receiver<bool>) -> RegisteredFixture {
    registered_fixture_with_associated_uri(stop, "<sip:authorized@ims.example>").await
}
pub(crate) async fn registered_fixture_with_associated_uri(
    stop: watch::Receiver<bool>,
    associated: &str,
) -> RegisteredFixture {
    registered_fixture_with_registration_headers(
        stop,
        associated,
        Some("<sip:registered-route@ims.example;lr>"),
    )
    .await
}
pub(crate) async fn registered_fixture_with_registration_headers(
    stop: watch::Receiver<bool>,
    associated: &str,
    service_route: Option<&str>,
) -> RegisteredFixture {
    let associated = associated.to_owned();
    let route = service_route
        .map(|route| format!("Service-Route: {route}\r\n"))
        .unwrap_or_default();
    use proletarius_ims::{
        SecAgreeMode,
        security_policy::LocalEndpoints,
        stream::{SipStreamItem, SipStreamReader},
    };
    use proletarius_vowifi::{
        netstack_runtime::TunnelNetwork,
        netstack_tcp::TcpSocketRegistry,
        registration_coordinator::{
            RegisterAttempt, RegistrationPlan, RegistrationServices, register,
        },
        security_runtime::SecurityNetworkRuntime,
    };
    use tokio::io::AsyncWriteExt;
    let config = |address: &str| proletarius_vowifi::TunnelConfiguration {
        ipv4_addresses: vec![address.parse().unwrap()],
        ..Default::default()
    };
    let local_network = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
    let remote_network = TunnelNetwork::new(&config("10.20.30.9")).unwrap();
    let (local_wake_tx, local_wake) = watch::channel(0u64);
    let (remote_wake_tx, remote_wake) = watch::channel(0u64);
    let (local_tcp, local_registry) = TcpSocketRegistry::new(local_wake_tx.clone());
    let (remote_tcp, remote_registry) = TcpSocketRegistry::new(remote_wake_tx);
    let (security, _unserviced) = SecurityNetworkRuntime::new(
        |_: &mut smoltcp::iface::Interface, _: &mut smoltcp::iface::SocketSet<'static>| {
            Ok::<(), std::convert::Infallible>(())
        },
        local_wake_tx,
    );
    let (local_out, remote_in) = mpsc::channel(64);
    let (remote_out, local_in) = mpsc::channel(64);
    let (network_stop, network_stopped) = watch::channel(false);
    let local = tokio::spawn(async move {
        local_network
            .run_managed(
                smoltcp::iface::SocketSet::new(vec![]),
                local_in,
                local_out,
                network_stopped.clone(),
                local_wake,
                local_registry,
            )
            .await
            .unwrap();
    });
    let remote_stop = network_stop.subscribe();
    let remote = tokio::spawn(async move {
        remote_network
            .run_managed(
                smoltcp::iface::SocketSet::new(vec![]),
                remote_in,
                remote_out,
                remote_stop,
                remote_wake,
                remote_registry,
            )
            .await
            .unwrap();
    });
    let (ready, listen_ready) = oneshot::channel();
    let (frames, output) = mpsc::channel(32);
    let (replies, mut input) = mpsc::channel::<Zeroizing<Vec<u8>>>(32);
    let peer = tokio::spawn(async move {
        let listener = remote_tcp
            .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
            .await
            .unwrap();
        ready.send(()).unwrap();
        let stream = listener.accept().await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = SipStreamReader::new(read);
        let Some(SipStreamItem::Message(registration)) = reader.next().await.unwrap() else {
            panic!("REGISTER");
        };
        assert!(registration.starts_with(b"REGISTER "));
        write.write_all(format!("SIP/2.0 200 OK\r\nCall-ID: register-fixture\r\nCSeq: 1 REGISTER\r\nP-Associated-URI: {associated}\r\n{route}Content-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
        loop {
            tokio::select! {
                frame=reader.next()=>match frame.unwrap() {Some(SipStreamItem::Message(bytes))=>{if frames.send(bytes).await.is_err(){break;}},Some(_)=>{},None=>break},
                reply=input.recv()=>match reply {Some(bytes)=>write.write_all(&bytes).await.unwrap(),None=>break},
            }
        }
        reader.into_inner().unsplit(write).close().await.unwrap();
    });
    listen_ready.await.unwrap();
    let (registration_events, _) = mpsc::channel(16);
    let session=register(RegistrationServices::new(&local_tcp,&security,&registration_events),
        RegistrationPlan {call_id:"register-fixture",initial_cseq:1,sec_agree:SecAgreeMode::Disabled,plain_peer:"10.20.30.9:6060".parse().unwrap(),plain_local_port:5064,local_ip:"10.0.0.2".parse().unwrap(),
            local_endpoints:LocalEndpoints {spi_c:1,spi_s:2,port_c:5064,port_s:5063},client_mechanisms:&[],strict_security:false},
        Instant::now()+Duration::from_secs(5),stop,
        |attempt|{let RegisterAttempt::Initial {cseq,..}=attempt else {panic!("unexpected auth");};
            Ok(Zeroizing::new(format!("REGISTER sip:ims.example SIP/2.0\r\nVia: SIP/2.0/TCP 10.0.0.2:5064;branch=z9hG4bKregister\r\nFrom: <sip:reader@ims.example>;tag=r\r\nTo: <sip:reader@ims.example>\r\nCall-ID: register-fixture\r\nCSeq: {cseq} REGISTER\r\nContent-Length: 0\r\n\r\n").into_bytes()))},
        |_|async {Err::<proletarius_ims::aka_exchange::AuthenticatedChallenge,_>("unexpected auth")}).await.unwrap_or_else(|_|panic!("registration"));
    let snapshot = session
        .message_context(
            "sip:configured@ims.example",
            &[format!("<sip:{};lr>", session.signaling_peer())],
        )
        .unwrap();
    RegisteredFixture {
        session: Some(session),
        snapshot,
        frames: output,
        replies: Some(replies),
        network_stop,
        peer,
        local,
        remote,
    }
}
fn sip_reply(request: &proletarius_sip::Request, status: u16) -> Zeroizing<Vec<u8>> {
    let mut headers = request.data.headers.clone();
    headers.set(Header::new("Content-Length", "0").unwrap());
    let via = headers.get("Via").unwrap().value().to_owned();
    headers.set(Header::new("Via", format!("{via};received=192.0.2.1;rport=5064")).unwrap());
    Zeroizing::new(
        Message::Response(Response {
            status,
            reason: "Fixture".into(),
            data: MessageData::new("SIP/2.0", headers, vec![]),
        })
        .encode(),
    )
}
#[tokio::test(flavor = "current_thread")]
async fn real_registered_actor_accepts_multipart_once_with_registered_routes_and_sms_center() {
    real_exchange(vec![202, 200]).await;
}
#[tokio::test(flavor = "current_thread")]
async fn real_registered_actor_partial_rejection_preserves_accepted_count_without_replay() {
    real_exchange(vec![200, 403]).await;
}
async fn real_exchange(statuses: Vec<u16>) {
    use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey};
    use proletarius_vowifi::sip_session_runtime::RegisteredSipRuntime;
    let mut supervisor = NativeVoWiFiSupervisor::default();
    let key = ReaderSessionKey::new("reader", "8901234567890123456").unwrap();
    let permit = supervisor.reserve(key.clone()).unwrap();
    supervisor.mark_running(&permit).unwrap();
    let session_generation = permit.generation();
    let cancel = permit.cancellation_sender();
    let mut fixture = registered_fixture(permit.stop_signal()).await;
    let session = fixture.session.take().unwrap();
    let (commands, command_input, events, mut received) = crate::native_ims_actor::channels();
    let (_lease, lost) = watch::channel(false);
    let owner = Arc::new(
        NativeReportOwner::new(Service::default(), "native-real-originating-0001".into()).unwrap(),
    );
    let mut originating = Originating::new(
        owner,
        SessionConfig {
            generation: session_generation,
            snapshot: &fixture.snapshot,
            minimum_cseq: 74,
            smsc: "+447802002606",
            target: "sip:smsc@ims.example",
            access_network_info: None,
        },
        commands,
        permit.stop_signal(),
        lost.clone(),
    )
    .unwrap();
    let actor = tokio::spawn(async move {
        crate::native_ims_actor::run(
            RegisteredSipRuntime::new(session),
            &key,
            permit,
            lost,
            command_input,
            events,
        )
        .await
    });
    let start = originating
        .start(TextSend {
            generation: session_generation,
            ..input(&"A".repeat(161))
        })
        .unwrap();
    assert_eq!(start.total_parts, 2);
    let mut part = 0;
    let mut references = vec![];
    let mut cseqs = vec![];
    let deadline = Instant::now() + Duration::from_secs(5);
    while originating.is_busy() {
        tokio::select! {
            frame=fixture.frames.recv()=>{
                let bytes=frame.unwrap();let (Message::Request(request),used)=Parser::default().parse(&bytes,true).unwrap() else {panic!("outgoing MESSAGE");};assert_eq!(used,bytes.len());
                assert_eq!(request.method,proletarius_sip::Method::MESSAGE);assert_eq!(request.uri.to_string(),"sip:smsc@ims.example");
                assert!(request.data.headers.get("From").unwrap().value().contains("sip:authorized@ims.example"));
                assert_eq!(request.data.headers.get("Route").unwrap().value(),"<sip:registered-route@ims.example;lr>");
                assert_eq!(request.data.body[4],0x91,"SMSC must stay in RP destination");
                references.push(request.data.body[1]);cseqs.push(request.data.headers.get("CSeq").unwrap().value().split_ascii_whitespace().next().unwrap().parse::<u32>().unwrap());
                assert!(part<statuses.len(),"accepted/unknown part was retransmitted");
                fixture.replies.as_ref().unwrap().send(sip_reply(&request,100)).await.unwrap();
                fixture.replies.as_ref().unwrap().send(sip_reply(&request,statuses[part])).await.unwrap();part+=1;
            },
            event=received.recv()=>{let event=event.unwrap();assert!(originating.process(&event).unwrap());},
            _=tokio::time::sleep_until(originating.next_deadline().unwrap_or(deadline))=>{originating.tick().unwrap();},
            _=tokio::time::sleep_until(deadline)=>panic!("multipart exchange did not drain"),
        }
    }
    assert_eq!(part, 2);
    assert_eq!(references[1], references[0].wrapping_add(1));
    assert!(cseqs[1] > cseqs[0] && cseqs[0] > 74);
    let receipt = originating.snapshot(BATCH).unwrap();
    if statuses[1] < 300 {
        assert_eq!(receipt.phase, Phase::Accepted);
        assert_eq!(receipt.accepted_parts, 2);
        assert!(!receipt.outcome_unknown());
    } else {
        assert_eq!(receipt.phase, Phase::Rejected);
        assert_eq!(receipt.accepted_parts, 1);
        assert_eq!(receipt.sip_status, Some(403));
        assert!(receipt.outcome_unknown());
    }
    assert_eq!(
        originating.start(input("must not replay")).unwrap_err(),
        "sms_batch_already_started"
    );
    originating.stop();
    cancel.send_replace(true);
    let exit = actor.await.unwrap();
    assert!(exit.cleanup.is_ok());
    assert!(
        fixture.frames.try_recv().is_err(),
        "teardown sent another SMS"
    );
    fixture.finish().await;
}

#[test]
fn explicit_cancel_after_queue_submission_does_not_cancel_another_sms_or_replay_the_part() {
    let mut f = fixture(4);
    f.outgoing.start(input(&"A".repeat(161))).unwrap();
    let (first, reply) = sent(&mut f);
    assert_eq!(
        f.outgoing.cancel(generation(2), BATCH).unwrap_err(),
        "native_sms_session_mismatch"
    );
    assert_eq!(
        f.outgoing.snapshot(BATCH).unwrap().phase,
        Phase::AwaitingResponse
    );
    assert_eq!(
        f.outgoing.cancel(generation(1), BATCH).unwrap().phase,
        Phase::OutcomeUnknown
    );
    drop(reply);
    let mut second = input("next SMS");
    second.batch_id = "native-originating-batch-0002".into();
    f.outgoing.start(second).unwrap();
    let (_next, _next_reply) = sent(&mut f);
    assert!(
        f.outgoing
            .process(&response(&first, 200, generation(1)))
            .unwrap()
    );
    assert_eq!(f.outgoing.snapshot(BATCH).unwrap().phase, Phase::Partial);
    assert_eq!(
        f.outgoing
            .snapshot("native-originating-batch-0002")
            .unwrap()
            .phase,
        Phase::AwaitingResponse
    );
    assert!(f.outgoing.is_busy());
    assert!(f.commands.try_recv().is_err());
}
#[test]
fn stop_racing_first_acceptance_retains_partial_result_and_never_queues_second_part() {
    let mut f = fixture(4);
    f.outgoing.start(input(&"A".repeat(161))).unwrap();
    let (request, _reply) = sent(&mut f);
    f.stop.send_replace(true);
    assert!(
        f.outgoing
            .process(&response(&request, 200, generation(1)))
            .unwrap()
    );
    let receipt = f.outgoing.snapshot(BATCH).unwrap();
    assert_eq!(receipt.phase, Phase::Partial);
    assert_eq!(receipt.accepted_parts, 1);
    assert!(receipt.outcome_unknown());
    assert!(f.commands.try_recv().is_err());
}
#[test]
fn network_rp_error_is_separate_from_sip_acceptance_and_cannot_change_another_batch() {
    let mut f = fixture(4);
    f.outgoing.start(input("BAL")).unwrap();
    let (request, reply) = sent(&mut f);
    drop(reply);
    f.outgoing.tick().unwrap();
    let reference = request.data.body[1];
    assert!(
        f.outgoing
            .process(&report(&request, reference, Some(21)))
            .unwrap()
    );
    assert_eq!(
        f.outgoing.snapshot(BATCH).unwrap().rp_results,
        vec![(1, RpResult::Error(21))]
    );
    assert_eq!(
        f.outgoing.snapshot(BATCH).unwrap().phase,
        Phase::OutcomeUnknown
    );
    assert!(f.commands.try_recv().is_err());
    let mut duplicate = report(&request, reference, None);
    if let RegisteredSipEvent::Message { message, .. } = &mut duplicate.event
        && let RegisteredSipMessage::Request { headers, .. } = message.as_mut()
    {
        headers.push(Header::new("In-Reply-To", "unrelated").unwrap());
    }
    assert!(!f.outgoing.process(&duplicate).unwrap());
}

#[test]
fn queued_unsubmitted_sms_wakes_when_capacity_becomes_available_without_a_sip_event() {
    let mut f = fixture(1);
    let (reply, _) = oneshot::channel();
    f.outgoing
        .commands
        .try_send(Command::Respond {
            generation: generation(1),
            connection: SipConnectionId::Primary,
            request: Box::new(RegisteredSipMessage::Response {
                status: 200,
                reason: "marker".into(),
                version: "SIP/2.0".into(),
                headers: Headers::default(),
                body: Zeroizing::new(vec![]),
            }),
            status: 200,
            reason: "marker".into(),
            reply,
        })
        .unwrap_or_else(|_| panic!("queue capacity"));
    assert_eq!(
        f.outgoing.start(input("BAL")).unwrap().phase,
        Phase::Prepared
    );
    assert!(f.outgoing.next_deadline().unwrap() <= Instant::now() + Duration::from_millis(26));
    f.commands.try_recv().unwrap();
    f.outgoing.tick().unwrap();
    let (_, reply) = sent(&mut f);
    drop(reply);
    f.outgoing.tick().unwrap();
    assert_eq!(
        f.outgoing.snapshot(BATCH).unwrap().phase,
        Phase::OutcomeUnknown
    );
    assert!(f.commands.try_recv().is_err());
}

#[tokio::test]
async fn optional_sms_routing_does_not_block_registered_session_but_send_is_rejected() {
    let (stop, stopped) = watch::channel(false);
    let fixture = registered_fixture(stopped.clone()).await;
    let (_lease, lost) = watch::channel(false);
    for (index, (smsc, target)) in [
        ("", "sip:smsc@ims.example"),
        ("+447802002606", ""),
        ("", ""),
    ]
    .into_iter()
    .enumerate()
    {
        let (commands, mut input_queue) = mpsc::channel(4);
        let owner = Arc::new(
            NativeReportOwner::new(
                Service::default(),
                format!("missing-route-session-{index:04}"),
            )
            .unwrap(),
        );
        let mut originating = Originating::new(
            owner,
            SessionConfig {
                generation: generation(1),
                snapshot: &fixture.snapshot,
                minimum_cseq: 74,
                smsc,
                target,
                access_network_info: None,
            },
            commands,
            stopped.clone(),
            lost.clone(),
        )
        .unwrap();
        assert_eq!(originating.generation(), generation(1));
        assert_eq!(
            originating.start(input("BAL")).unwrap_err(),
            "native_sms_routing_unavailable"
        );
        assert!(!originating.is_busy());
        assert!(originating.snapshots().is_empty());
        assert!(originating.next_deadline().is_none());
        assert!(input_queue.try_recv().is_err());
    }
    for (index, (smsc, target)) in [
        (
            "+447802002606".to_owned(),
            "sip:smsc@ims.example\r\nInjected: true".to_owned(),
        ),
        ("1".repeat(65), "sip:smsc@ims.example".to_owned()),
        ("+447802002606".to_owned(), "x".repeat(2049)),
    ]
    .into_iter()
    .enumerate()
    {
        let (commands, _) = mpsc::channel(4);
        let owner = Arc::new(
            NativeReportOwner::new(
                Service::default(),
                format!("invalid-route-session-{index:04}"),
            )
            .unwrap(),
        );
        assert!(matches!(
            Originating::new(
                owner,
                SessionConfig {
                    generation: generation(1),
                    snapshot: &fixture.snapshot,
                    minimum_cseq: 74,
                    smsc: &smsc,
                    target: &target,
                    access_network_info: None,
                },
                commands,
                stopped.clone(),
                lost.clone()
            ),
            Err("native_sms_routing_unavailable")
        ));
    }
    stop.send_replace(true);
    fixture.finish().await;
}
