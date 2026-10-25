use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
#[derive(Default)]
struct FakeMedia {
    allocated: AtomicUsize,
    allocate_fail: AtomicBool,
    started: AtomicUsize,
    start_modes: Mutex<Vec<bool>>,
    stopped: AtomicUsize,
    stop_attempts: AtomicUsize,
    stop_fail: AtomicBool,
    configured: Mutex<Vec<AudioNegotiation>>,
    prepared: Mutex<Vec<AudioNegotiation>>,
    local_offers: Mutex<Vec<Vec<u8>>>,
    prepare_local_offer_fail: AtomicBool,
    start_fail: AtomicBool,
    discards: AtomicUsize,
    prepare_fail: AtomicBool,
    configure_fail: AtomicBool,
    discard_fail: AtomicBool,
    events: Mutex<Vec<String>>,
    terminal_error: Mutex<Option<Failure>>,
    transmit_held: AtomicBool,
    digits: Mutex<Vec<char>>,
    drain: Mutex<Option<Arc<tokio::sync::Notify>>>,
}
impl Media for FakeMedia {
    fn prepare_local_offer(self: Arc<Self>, _: CallBinding, offer: Vec<u8>) -> MediaFuture<()> {
        Box::pin(async move {
            self.events
                .lock()
                .unwrap()
                .push("prepare_local_offer".into());
            self.local_offers.lock().unwrap().push(offer);
            if self.prepare_local_offer_fail.load(Ordering::SeqCst) {
                return Err(Failure::known("ims_media_prepare_failed"));
            }
            Ok(())
        })
    }
    fn hold_transmit(self: Arc<Self>, _: CallBinding, hold: bool) -> MediaFuture<()> {
        Box::pin(async move {
            self.events.lock().unwrap().push(format!("hold{hold}"));
            self.transmit_held.store(hold, Ordering::SeqCst);
            Ok(())
        })
    }
    fn terminal_failure(&self, _: &CallBinding) -> Option<Failure> {
        self.terminal_error.lock().unwrap().clone()
    }
    fn prepare(self: Arc<Self>, _: CallBinding, remote: AudioNegotiation) -> MediaFuture<()> {
        Box::pin(async move {
            self.events.lock().unwrap().push("prepare".into());
            if self.prepare_fail.load(Ordering::SeqCst) {
                return Err(Failure::known("ims_media_prepare_failed"));
            }
            self.prepared.lock().unwrap().push(remote);
            Ok(())
        })
    }
    fn discard(self: Arc<Self>, _: CallBinding) -> MediaFuture<()> {
        Box::pin(async move {
            self.events.lock().unwrap().push("discard".into());
            self.discards.fetch_add(1, Ordering::SeqCst);
            if self.discard_fail.load(Ordering::SeqCst) {
                return Err(Failure::known("ims_media_discard_failed"));
            }
            Ok(())
        })
    }
    fn allocate(self: Arc<Self>, _: CallBinding) -> MediaFuture<LocalMedia> {
        Box::pin(async move {
            if self.allocate_fail.load(Ordering::SeqCst) {
                return Err(Failure::known("ims_media_allocate_failed"));
            }
            self.allocated.fetch_add(1, Ordering::SeqCst);
            Ok(LocalMedia {
                ip: "192.0.2.10".parse().unwrap(),
                port: 49170,
                audio_port: 49000,
                audio_token: "fake-token".into(),
            })
        })
    }
    fn configure(self: Arc<Self>, _: CallBinding, remote: AudioNegotiation) -> MediaFuture<()> {
        Box::pin(async move {
            self.events.lock().unwrap().push("configure".into());
            if self.configure_fail.load(Ordering::SeqCst) {
                return Err(Failure::known("ims_media_commit_failed"));
            }
            self.configured.lock().unwrap().push(remote);
            Ok(())
        })
    }
    fn start(self: Arc<Self>, _: CallBinding, early: bool) -> MediaFuture<()> {
        Box::pin(async move {
            self.start_modes.lock().unwrap().push(early);
            self.started.fetch_add(1, Ordering::SeqCst);
            if self.start_fail.load(Ordering::SeqCst) {
                return Err(Failure::known("ims_media_start_failed"));
            }
            Ok(())
        })
    }
    fn stop(self: Arc<Self>, _: CallBinding) -> MediaFuture<()> {
        Box::pin(async move {
            let hold = self.drain.lock().unwrap().clone();
            if let Some(hold) = hold {
                hold.notified().await;
            }
            self.stop_attempts.fetch_add(1, Ordering::SeqCst);
            if self.stop_fail.load(Ordering::SeqCst) {
                return Err(Failure::known("ims_media_stop_failed"));
            }
            self.stopped.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn dtmf(self: Arc<Self>, _: CallBinding, digit: char) -> MediaFuture<()> {
        Box::pin(async move {
            self.digits.lock().unwrap().push(digit);
            Ok(())
        })
    }
}
struct Wire {
    request: Option<Request>,
    response: Option<Response>,
    connection: SipConnectionId,
}
fn config() -> Config {
    Config {
        local_ip: "192.0.2.10".parse().unwrap(),
        port_c: 5060,
        contact: "<sip:a@192.0.2.10:5061>".into(),
        from_uri: "sip:a@ims.test".into(),
        preferred_identity: "sip:a@ims.test".into(),
        route: "<sip:edge.ims.test;lr>".into(),
        service_routes: vec!["<sip:registered.ims.test;lr>".into()],
        encoded_icsi: "urn%3Aurn-7%3A3gpp-service.ims.icsi.mmtel".into(),
        phone_context: String::new(),
        home_domain: "ims.test".into(),
        realm: "ims.test".into(),
        user_agent: "native-test".into(),
        security_verify: String::new(),
        pani: String::new(),
        early_media_header: false,
        evs_only: false,
        sms_only: false,
        ussd_service_selector: false,
    }
}
fn fixture() -> (
    Service,
    Arc<FakeMedia>,
    mpsc::UnboundedReceiver<Wire>,
    tokio::task::JoinHandle<()>,
) {
    fixture_with_ack_failure(false)
}
fn fixture_with_ack_failure(
    fail_ack: bool,
) -> (
    Service,
    Arc<FakeMedia>,
    mpsc::UnboundedReceiver<Wire>,
    tokio::task::JoinHandle<()>,
) {
    let (commands, mut input) = mpsc::channel(32);
    let (sent, output) = mpsc::unbounded_channel();
    let media = Arc::new(FakeMedia::default());
    let media_for_wire = media.clone();
    let task = tokio::spawn(async move {
        while let Some(command) = input.recv().await {
            match command {
                Command::SendUntracked {
                    generation,
                    request,
                    reply,
                } => {
                    assert_eq!(generation.value(), 7);
                    let ack_failed = fail_ack && request.method == Method::ACK;
                    sent.send(Wire {
                        request: Some(request),
                        response: None,
                        connection: SipConnectionId::Primary,
                    })
                    .unwrap();
                    let _ = reply.send(if ack_failed {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "controlled ACK write failure",
                        ))
                    } else {
                        Ok(())
                    });
                }
                Command::RespondDialog {
                    generation,
                    connection,
                    response,
                    reply,
                } => {
                    assert_eq!(generation.value(), 7);
                    media_for_wire
                        .events
                        .lock()
                        .unwrap()
                        .push(format!("response{}", response.status));
                    sent.send(Wire {
                        request: None,
                        response: Some(response),
                        connection,
                    })
                    .unwrap();
                    let _ = reply.send(Ok(()));
                }
                _ => panic!("voice must never borrow tracked SMS transaction"),
            }
        }
    });
    let service = Service::new(
        SessionGeneration::from_value(7).unwrap(),
        config(),
        commands,
        media.clone(),
    )
    .unwrap();
    (service, media, output, task)
}
fn g() -> SessionGeneration {
    SessionGeneration::from_value(7).unwrap()
}
fn offer() -> Vec<u8> {
    sdp::build_local_sdp(
        9,
        "192.0.2.20".parse().unwrap(),
        49172,
        false,
        None,
        None,
        None,
        false,
    )
    .unwrap()
    .into_bytes()
}
fn inbound(id: &str, seq: u32, method: Method, localtag: Option<&str>) -> Request {
    let mut headers = Headers::default();
    for (name, value) in [
        (
            "Via",
            "SIP/2.0/TCP peer.ims.test;branch=z9hG4bKin;rport".into(),
        ),
        ("From", "<sip:b@ims.test>;tag=remote".into()),
        (
            "To",
            format!(
                "<sip:a@ims.test>{}",
                localtag.map(|s| format!(";tag={s}")).unwrap_or_default()
            ),
        ),
        ("Call-ID", id.into()),
        ("CSeq", format!("{seq} {}", method.as_str())),
        ("Contact", "<sip:b@peer.ims.test>".into()),
        ("Record-Route", "<sip:edge.ims.test;lr>".into()),
    ] {
        push(&mut headers, name, &value).unwrap();
    }
    let body = if method == Method::INVITE {
        offer()
    } else {
        vec![]
    };
    Request {
        method,
        uri: Uri::parse("sip:a@ims.test").unwrap(),
        data: MessageData::new("SIP/2.0", headers, body),
    }
}
fn in_dialog(id: &str, seq: u32, method: Method, localtag: &str) -> Request {
    let mut request = inbound(id, seq, method, Some(localtag));
    request
        .data
        .headers
        .set(Header::new("From", "<sip:b@ims.test>;tag=remote-tag").unwrap());
    request
}
fn request_event(request: Request) -> Event {
    Event {
        generation: g(),
        reply_state: crate::native_ims_actor::IncomingReply::Unanswered,
        event: RegisteredSipEvent::Message {
            connection: SipConnectionId::Primary,
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
fn response_event(invite: &Request, status: u16, body: Vec<u8>, rseq: Option<u32>) -> Event {
    let mut response = reply_for(
        invite,
        status,
        "fixture",
        Some("remote-tag"),
        body,
        Some("<sip:b@peer.ims.test>"),
    )
    .unwrap();
    push(
        &mut response.data.headers,
        "Record-Route",
        "<sip:edge-a.ims.test;lr>",
    )
    .unwrap();
    push(
        &mut response.data.headers,
        "Record-Route",
        "<sip:edge-b.ims.test;lr>",
    )
    .unwrap();
    if let Some(rseq) = rseq {
        push(&mut response.data.headers, "Require", "100rel").unwrap();
        push(&mut response.data.headers, "RSeq", &rseq.to_string()).unwrap();
    }
    // Carrier adds received/rport facts without changing the transaction branch.
    let via = response.data.headers.get("Via").unwrap().value().to_owned();
    response
        .data
        .headers
        .set(Header::new("Via", format!("{via};received=192.0.2.10")).unwrap());
    Event {
        generation: g(),
        reply_state: crate::native_ims_actor::IncomingReply::Unanswered,
        event: RegisteredSipEvent::Message {
            connection: SipConnectionId::Primary,
            message: Box::new(RegisteredSipMessage::Response {
                status,
                reason: response.reason,
                version: response.data.version,
                headers: response.data.headers,
                body: zeroize::Zeroizing::new(response.data.body),
            }),
            response_ticket: None,
        },
    }
}
fn response_with_timer(request: &Request, status: u16, timer: Option<&str>) -> Event {
    let mut event = response_event(
        request,
        status,
        if request.method == Method::INVITE {
            offer()
        } else {
            Vec::new()
        },
        None,
    );
    if let Some(timer) = timer {
        let RegisteredSipEvent::Message { message, .. } = &mut event.event else {
            unreachable!()
        };
        let RegisteredSipMessage::Response { headers, .. } = message.as_mut() else {
            unreachable!()
        };
        push(headers, "Session-Expires", timer).unwrap();
    }
    let RegisteredSipEvent::Message { message, .. } = &mut event.event else {
        unreachable!()
    };
    let RegisteredSipMessage::Response { headers, .. } = message.as_mut() else {
        unreachable!()
    };
    push(headers, "Allow", "INVITE, ACK, UPDATE").unwrap();
    event
}

fn response_without_update(request: &Request, status: u16, timer: Option<&str>) -> Event {
    let mut event = response_event(
        request,
        status,
        if request.method == Method::INVITE && (200..300).contains(&status) {
            offer()
        } else {
            Vec::new()
        },
        None,
    );
    let RegisteredSipEvent::Message { message, .. } = &mut event.event else {
        unreachable!()
    };
    let RegisteredSipMessage::Response { headers, .. } = message.as_mut() else {
        unreachable!()
    };
    if let Some(timer) = timer {
        push(headers, "Session-Expires", timer).unwrap();
    }
    event
}

fn response_with_min_se(request: &Request, minimum: Option<&str>) -> Event {
    let mut event = response_event(request, 422, Vec::new(), None);
    if let Some(minimum) = minimum {
        let RegisteredSipEvent::Message { message, .. } = &mut event.event else {
            unreachable!()
        };
        let RegisteredSipMessage::Response { headers, .. } = message.as_mut() else {
            unreachable!()
        };
        push(headers, "Min-SE", minimum).unwrap();
    }
    event
}

#[tokio::test]
async fn initial_invite_422_retries_new_transactions_without_reallocating_media() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let first = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(cseq(&first.data.headers).unwrap().0, 1);
    service
        .handle_event(&response_with_min_se(&first, Some("3600;source=proxy")))
        .await
        .unwrap();
    let first_ack = wire.recv().await.unwrap().request.unwrap();
    let second = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(first_ack.method, Method::ACK);
    assert_eq!(cseq(&first_ack.data.headers).unwrap().0, 1);
    assert!(same_via(&first_ack.data.headers, &first.data.headers));
    assert_eq!(second.method, Method::INVITE);
    assert_eq!(cseq(&second.data.headers).unwrap().0, 2);
    assert!(!same_via(&first.data.headers, &second.data.headers));
    for name in ["Call-ID", "From", "To"] {
        assert_eq!(
            single(&first.data.headers, name).unwrap(),
            single(&second.data.headers, name).unwrap()
        );
    }
    assert_eq!(second.data.body, first.data.body);
    assert_eq!(single(&second.data.headers, "Min-SE").unwrap(), "3600");
    assert_eq!(
        single(&second.data.headers, "Session-Expires").unwrap(),
        "3600;refresher=uac"
    );
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    assert_eq!(service.call.as_ref().unwrap().binding.invite_cseq, 1);

    service
        .handle_event(&response_with_min_se(&first, Some("3600;source=proxy")))
        .await
        .unwrap();
    let duplicate_ack = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(duplicate_ack.method, Method::ACK);
    assert!(same_via(
        &duplicate_ack.data.headers,
        &first_ack.data.headers
    ));
    assert!(wire.try_recv().is_err());

    service
        .handle_event(&response_with_min_se(&second, Some("4000")))
        .await
        .unwrap();
    let second_ack = wire.recv().await.unwrap().request.unwrap();
    let third = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(cseq(&second_ack.data.headers).unwrap().0, 2);
    assert_eq!(cseq(&third.data.headers).unwrap().0, 3);
    assert_eq!(single(&third.data.headers, "Min-SE").unwrap(), "4000");
    assert_eq!(
        single(&third.data.headers, "Session-Expires").unwrap(),
        "4000;refresher=uac"
    );
    service
        .handle_event(&response_with_timer(
            &third,
            200,
            Some("4000;refresher=uac"),
        ))
        .await
        .unwrap();
    let final_ack = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(final_ack.method, Method::ACK);
    assert_eq!(cseq(&final_ack.data.headers).unwrap().0, 3);
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_invite_422_without_increasing_minimum_does_not_loop() {
    for minimum in [None, Some("90"), Some("1800"), Some("invalid")] {
        let (mut service, media, mut wire, task) = fixture();
        service.dial(g(), "+447700900123").await.unwrap();
        let invite = wire.recv().await.unwrap().request.unwrap();
        service
            .handle_event(&response_with_min_se(&invite, minimum))
            .await
            .unwrap();
        let ack = wire.recv().await.unwrap().request.unwrap();
        assert_eq!(ack.method, Method::ACK);
        assert!(wire.try_recv().is_err());
        assert_eq!(service.snapshot.state, "failed");
        assert_eq!(service.snapshot.last_error, "ims_call_rejected_422");
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        service.shutdown(g()).await.unwrap();
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_invite_422_retry_rejects_a_short_final_session_interval() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let first = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_with_min_se(&first, Some("3600")))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    let retry = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_with_timer(&retry, 200, Some("180;refresher=uac")))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_session_timer_invalid");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_invite_422_after_cancel_does_not_redial() {
    let (mut service, _, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let invite = wire.recv().await.unwrap().request.unwrap();
    service.hangup(g()).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::CANCEL
    );
    service
        .handle_event(&response_with_min_se(&invite, Some("3600")))
        .await
        .unwrap();
    let ack = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(ack.method, Method::ACK);
    assert!(wire.try_recv().is_err());
    assert_eq!(service.snapshot.state, "ended");
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_invite_422_retry_budget_is_bounded() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let mut invite = wire.recv().await.unwrap().request.unwrap();
    for (index, minimum) in [3600, 3700, 3800, 3900].into_iter().enumerate() {
        service
            .handle_event(&response_with_min_se(&invite, Some(&minimum.to_string())))
            .await
            .unwrap();
        assert_eq!(
            wire.recv().await.unwrap().request.unwrap().method,
            Method::ACK
        );
        invite = wire.recv().await.unwrap().request.unwrap();
        assert_eq!(cseq(&invite.data.headers).unwrap().0, index as u32 + 2);
    }
    service
        .handle_event(&response_with_min_se(&invite, Some("4000")))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert!(wire.try_recv().is_err());
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn cancel_after_initial_timer_retry_targets_the_new_invite_transaction() {
    let (mut service, _, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let first = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_with_min_se(&first, Some("3600")))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    let retry = wire.recv().await.unwrap().request.unwrap();
    service.hangup(g()).await.unwrap();
    let cancel = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(cancel.method, Method::CANCEL);
    assert_eq!(cseq(&cancel.data.headers).unwrap().0, 2);
    assert!(same_via(&cancel.data.headers, &retry.data.headers));
    service
        .handle_event(&response_event(&retry, 487, Vec::new(), None))
        .await
        .unwrap();
    let ack = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(ack.method, Method::ACK);
    assert_eq!(cseq(&ack.data.headers).unwrap().0, 2);
    assert_eq!(service.snapshot.state, "ended");
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_422_after_tagged_provisional_dialog_fails_closed() {
    let (mut service, _, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let invite = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_event(&invite, 180, Vec::new(), None))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "early");
    service
        .handle_event(&response_with_min_se(&invite, Some("3600")))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert!(wire.try_recv().is_err());
    assert_eq!(service.snapshot.state, "failed");
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn local_session_timer_sends_update_at_half_interval_and_rearms_on_2xx() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_with_timer(
            &initial,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert_eq!(service.snapshot.state, "active");
    let first_media = media.configured.lock().unwrap().clone();
    let due = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), due).await.unwrap();
    let update = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(update.method, Method::UPDATE);
    assert!(update.data.body.is_empty());
    assert_eq!(
        single(&update.data.headers, "Session-Expires").unwrap(),
        "180;refresher=uac"
    );
    assert_eq!(single(&update.data.headers, "Min-SE").unwrap(), "90");
    assert_eq!(service.snapshot.state, "active");
    service
        .handle_event(&response_with_timer(
            &update,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    let timer = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap();
    assert!(timer.pending.is_none());
    assert!(timer.refresh_at.unwrap() > due);
    assert_eq!(*media.configured.lock().unwrap(), first_media);
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn session_refresh_uses_reinvite_without_peer_update_support_and_acks_retransmission() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_without_update(
            &initial,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    let original_media = media.configured.lock().unwrap().clone();
    let due = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), due).await.unwrap();
    let refresh = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(refresh.method, Method::INVITE);
    assert_eq!(refresh.data.body, initial.data.body);
    assert_eq!(
        single(&refresh.data.headers, "Content-Type").unwrap(),
        "application/sdp"
    );
    assert!(cseq(&refresh.data.headers).unwrap().0 > cseq(&initial.data.headers).unwrap().0);
    let response = response_without_update(&refresh, 200, Some("180;refresher=uac"));
    service.handle_event(&response).await.unwrap();
    let ack = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(ack.method, Method::ACK);
    assert_eq!(
        cseq(&ack.data.headers).unwrap().0,
        cseq(&refresh.data.headers).unwrap().0
    );
    assert_eq!(*media.configured.lock().unwrap(), original_media);
    assert!(
        service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .unwrap()
            .refresh_at
            .is_some()
    );
    service.handle_event(&response).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

async fn timed_out_refresh() -> (
    Service,
    Arc<FakeMedia>,
    mpsc::UnboundedReceiver<Wire>,
    tokio::task::JoinHandle<()>,
    Request,
) {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_without_update(
            &initial,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    let due = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), due).await.unwrap();
    let refresh = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(refresh.method, Method::INVITE);
    let deadline = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .deadline;
    service.tick(g(), deadline).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_session_refresh_timeout");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    (service, media, wire, task, refresh)
}

#[tokio::test]
async fn timed_out_refresh_late_2xx_is_acked_without_restarting_media() {
    let (mut service, media, mut wire, task, refresh) = timed_out_refresh().await;
    let response = response_without_update(&refresh, 200, Some("180;refresher=uac"));
    assert!(service.handle_event(&response).await.unwrap());
    let ack = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(ack.method, Method::ACK);
    assert_eq!(
        cseq(&ack.data.headers).unwrap().0,
        cseq(&refresh.data.headers).unwrap().0
    );
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_session_refresh_timeout");
    assert_eq!(media.started.load(Ordering::SeqCst), 1);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(service.call.as_ref().unwrap().session_timer.is_none());
    assert!(service.handle_event(&response).await.unwrap());
    assert_eq!(wire.recv().await.unwrap().request.unwrap(), ack);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn timed_out_refresh_late_rejection_uses_transaction_ack_without_retry() {
    let (mut service, media, mut wire, task, refresh) = timed_out_refresh().await;
    let response = response_with_min_se(&refresh, Some("360"));
    assert!(service.handle_event(&response).await.unwrap());
    let ack = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(ack.method, Method::ACK);
    assert_eq!(
        cseq(&ack.data.headers).unwrap().0,
        cseq(&refresh.data.headers).unwrap().0
    );
    assert!(same_via(&ack.data.headers, &refresh.data.headers));
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(service.call.as_ref().unwrap().session_timer.is_none());
    assert!(service.handle_event(&response).await.unwrap());
    assert_eq!(wire.recv().await.unwrap().request.unwrap(), ack);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn retired_timed_out_refresh_acks_late_2xx_without_touching_new_call() {
    let (mut service, media, mut wire, task, refresh) = timed_out_refresh().await;
    service.dial(g(), "+447700900124").await.unwrap();
    let new_invite = wire.recv().await.unwrap().request.unwrap();
    let before = service.snapshot();
    let response = response_without_update(&refresh, 200, Some("180;refresher=uac"));
    assert!(service.handle_event(&response).await.unwrap());
    let ack = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(ack.method, Method::ACK);
    assert_eq!(
        single(&ack.data.headers, "Call-ID").unwrap(),
        single(&refresh.data.headers, "Call-ID").unwrap()
    );
    assert_ne!(
        single(&ack.data.headers, "Call-ID").unwrap(),
        single(&new_invite.data.headers, "Call-ID").unwrap()
    );
    assert_eq!(
        serde_json::to_value(service.snapshot()).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(service.handle_event(&response).await.unwrap());
    assert_eq!(wire.recv().await.unwrap().request.unwrap(), ack);
    assert_eq!(
        serde_json::to_value(service.snapshot()).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn pending_refresh_reinvite_rejects_colliding_remote_update_without_losing_transaction() {
    let (mut service, _, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_without_update(
            &initial,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    wire.recv().await.unwrap();
    let due = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), due).await.unwrap();
    let refresh = wire.recv().await.unwrap().request.unwrap();
    let call_id = single(&initial.data.headers, "Call-ID").unwrap().to_owned();
    let local_tag = tag(single(&initial.data.headers, "From").unwrap()).unwrap();
    let update = in_dialog(&call_id, 2, Method::UPDATE, local_tag);
    service.handle_event(&request_event(update)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 491);
    assert!(
        service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .unwrap()
            .pending
            .is_some()
    );
    service
        .handle_event(&response_without_update(
            &refresh,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert_eq!(service.snapshot.state, "active");
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn session_refresh_reinvite_422_acks_and_retries_with_higher_minimum() {
    let (mut service, _, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_without_update(
            &initial,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    wire.recv().await.unwrap();
    let due = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), due).await.unwrap();
    let first = wire.recv().await.unwrap().request.unwrap();
    let response = response_with_min_se(&first, Some("360"));
    service.handle_event(&response).await.unwrap();
    let ack = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(ack.method, Method::ACK);
    assert!(same_via(&ack.data.headers, &first.data.headers));
    assert_eq!(
        cseq(&ack.data.headers).unwrap().0,
        cseq(&first.data.headers).unwrap().0
    );
    let retry_at = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), retry_at).await.unwrap();
    let retry = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(retry.method, Method::INVITE);
    assert_eq!(single(&retry.data.headers, "Min-SE").unwrap(), "360");
    assert!(cseq(&retry.data.headers).unwrap().0 > cseq(&first.data.headers).unwrap().0);
    service
        .handle_event(&response_without_update(
            &retry,
            200,
            Some("360;refresher=uac"),
        ))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn session_refresh_update_405_falls_back_to_reinvite() {
    let (mut service, _, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_with_timer(
            &initial,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    wire.recv().await.unwrap();
    let due = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), due).await.unwrap();
    let update = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(update.method, Method::UPDATE);
    service
        .handle_event(&response_without_update(&update, 405, None))
        .await
        .unwrap();
    let retry_at = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), retry_at).await.unwrap();
    let invite = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(invite.method, Method::INVITE);
    assert_eq!(invite.data.body, initial.data.body);
    service
        .handle_event(&response_without_update(
            &invite,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn session_refresh_reinvite_bad_sdp_acks_then_ends_dialog() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_without_update(
            &initial,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    wire.recv().await.unwrap();
    let due = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), due).await.unwrap();
    let invite = wire.recv().await.unwrap().request.unwrap();
    let mut bad = response_without_update(&invite, 200, Some("180;refresher=uac"));
    let RegisteredSipEvent::Message { message, .. } = &mut bad.event else {
        unreachable!()
    };
    let RegisteredSipMessage::Response { body, .. } = message.as_mut() else {
        unreachable!()
    };
    *body = zeroize::Zeroizing::new(b"not an SDP".to_vec());
    service.handle_event(&bad).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(
        service.snapshot.last_error,
        "ims_session_refresh_sdp_invalid"
    );
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn session_refresh_2xx_without_session_expires_disables_timer_without_ending_call() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_with_timer(&initial, 200, Some("180")))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    let timer = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap();
    let due = timer.refresh_at.unwrap();
    let old_termination = timer.termination_at;
    service.tick(g(), due).await.unwrap();
    let refresh = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(refresh.method, Method::UPDATE);
    service
        .handle_event(&response_with_timer(&refresh, 200, None))
        .await
        .unwrap();
    assert!(service.call.as_ref().unwrap().session_timer.is_none());
    service.tick(g(), old_termination).await.unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn session_refresh_2xx_requiring_timer_without_interval_ends_call() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_with_timer(
            &initial,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    wire.recv().await.unwrap(); // Initial ACK.
    let due = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), due).await.unwrap();
    let refresh = wire.recv().await.unwrap().request.unwrap();
    let mut malformed = response_with_timer(&refresh, 200, None);
    let RegisteredSipEvent::Message { message, .. } = &mut malformed.event else {
        unreachable!()
    };
    let RegisteredSipMessage::Response { headers, .. } = message.as_mut() else {
        unreachable!()
    };
    push(headers, "Require", "timer").unwrap();
    service.handle_event(&malformed).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.last_error, "ims_session_timer_invalid");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn session_refresh_422_raises_minimum_once_and_408_ends_call() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_with_timer(
            &initial,
            200,
            Some("180;refresher=uac"),
        ))
        .await
        .unwrap();
    wire.recv().await.unwrap(); // Initial ACK.
    let due = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), due).await.unwrap();
    let update = wire.recv().await.unwrap().request.unwrap();
    let mut too_small = response_with_timer(&update, 422, None);
    let RegisteredSipEvent::Message { message, .. } = &mut too_small.event else {
        unreachable!()
    };
    let RegisteredSipMessage::Response { headers, .. } = message.as_mut() else {
        unreachable!()
    };
    push(headers, "Min-SE", "360").unwrap();
    service.handle_event(&too_small).await.unwrap();
    let retry_at = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .refresh_at
        .unwrap();
    service.tick(g(), retry_at).await.unwrap();
    let retry = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(retry.method, Method::UPDATE);
    assert_eq!(single(&retry.data.headers, "Min-SE").unwrap(), "360");
    assert_eq!(
        single(&retry.data.headers, "Session-Expires").unwrap(),
        "360;refresher=uac"
    );
    assert!(cseq(&retry.data.headers).unwrap().0 > cseq(&update.data.headers).unwrap().0);
    service
        .handle_event(&response_with_timer(&retry, 200, Some("360;refresher=uac")))
        .await
        .unwrap();
    let timer = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap();
    assert_eq!(timer.min_se, 360);
    service.tick(g(), timer.refresh_at.unwrap()).await.unwrap();
    let retry = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(single(&retry.data.headers, "Min-SE").unwrap(), "360");
    service
        .handle_event(&response_with_timer(&retry, 408, None))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_session_refresh_rejected");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn remote_refresher_does_not_send_update_and_expires_without_remote_refresh() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_with_timer(
            &initial,
            200,
            Some("180;refresher=uas"),
        ))
        .await
        .unwrap();
    wire.recv().await.unwrap(); // Initial ACK.
    let timer = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap();
    assert!(!timer.local_refresher);
    assert!(timer.refresh_at.is_none());
    let termination = timer.termination_at;
    service
        .tick(g(), termination - Duration::from_secs(1))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    service.tick(g(), termination).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.last_error, "ims_session_expired");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn incoming_session_timer_is_answered_and_remote_update_extends_it() {
    let (mut service, media, mut wire, task) = fixture();
    let call_id = "incoming-session-timer";
    let mut invite = inbound(call_id, 14, Method::INVITE, None);
    push(
        &mut invite.data.headers,
        "Session-Expires",
        "240;refresher=uac",
    )
    .unwrap();
    push(&mut invite.data.headers, "Min-SE", "240").unwrap();
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 180);
    service.answer(g()).await.unwrap();
    let answer = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(answer.status, 200);
    assert_eq!(
        single(&answer.data.headers, "Session-Expires").unwrap(),
        "240;refresher=uac"
    );
    assert!(token(&answer.data.headers, "Require", "timer"));
    let pending = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap();
    let (retry_at, ack_deadline) = (pending.next_response_retry, pending.deadline);
    let expiry_at_receipt = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .expires_at;
    let configured = media.configured.lock().unwrap().clone();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(
        wire.try_recv().is_err(),
        "Accepted absorbs duplicate INVITE; only the TU timer retransmits200"
    );
    assert_eq!(service.snapshot.state, "connecting");
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), 1);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    let pending = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap();
    assert_eq!(pending.next_response_retry, retry_at);
    assert_eq!(pending.deadline, ack_deadline);
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .unwrap()
            .expires_at,
        expiry_at_receipt
    );
    service.tick(g(), retry_at).await.unwrap();
    let retransmission = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(
        proletarius_sip::Message::Response(retransmission.clone()).encode(),
        proletarius_sip::Message::Response(answer.clone()).encode()
    );
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .initial_answer
            .as_ref()
            .unwrap()
            .deadline,
        ack_deadline
    );
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .unwrap()
            .expires_at,
        expiry_at_receipt
    );
    assert_eq!(retransmission.status, 200);
    assert_eq!(retransmission.data.body, answer.data.body);
    assert_eq!(
        single(&retransmission.data.headers, "Session-Expires").unwrap(),
        single(&answer.data.headers, "Session-Expires").unwrap()
    );
    assert!(token(&retransmission.data.headers, "Require", "timer"));
    let local_tag = service.call.as_ref().unwrap().local_tag.clone();
    service
        .handle_event(&request_event(inbound(
            call_id,
            14,
            Method::ACK,
            Some(&local_tag),
        )))
        .await
        .unwrap();
    let original_expiry = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .expires_at;
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .unwrap()
            .min_se,
        240
    );
    assert!(
        !service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .unwrap()
            .local_refresher
    );
    let mut refresh = inbound(call_id, 15, Method::UPDATE, Some(&local_tag));
    push(
        &mut refresh.data.headers,
        "Session-Expires",
        "300;refresher=uac",
    )
    .unwrap();
    push(&mut refresh.data.headers, "Min-SE", "300").unwrap();
    let mut wrong_connection = request_event(refresh.clone());
    let RegisteredSipEvent::Message { connection, .. } = &mut wrong_connection.event else {
        unreachable!()
    };
    *connection = SipConnectionId::Incoming(99);
    service.handle_event(&wrong_connection).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 481);
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .unwrap()
            .expires_at,
        original_expiry
    );
    service.handle_event(&request_event(refresh)).await.unwrap();
    let response = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        single(&response.data.headers, "Session-Expires").unwrap(),
        "300;refresher=uac"
    );
    assert!(
        service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .unwrap()
            .expires_at
            >= original_expiry
    );
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .unwrap()
            .min_se,
        300
    );
    let previous_expiry = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .expires_at;
    let mut too_short = inbound(call_id, 16, Method::UPDATE, Some(&local_tag));
    push(
        &mut too_short.data.headers,
        "Session-Expires",
        "180;refresher=uac",
    )
    .unwrap();
    push(&mut too_short.data.headers, "Min-SE", "300").unwrap();
    service
        .handle_event(&request_event(too_short))
        .await
        .unwrap();
    let rejected = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(rejected.status, 422);
    assert_eq!(single(&rejected.data.headers, "Min-SE").unwrap(), "300");
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .unwrap()
            .expires_at,
        previous_expiry
    );
    let mut malformed = inbound(call_id, 17, Method::UPDATE, Some(&local_tag));
    push(&mut malformed.data.headers, "Require", "timer").unwrap();
    service
        .handle_event(&request_event(malformed))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 400);
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .unwrap()
            .expires_at,
        previous_expiry
    );
    service
        .handle_event(&request_event(inbound(
            call_id,
            18,
            Method::UPDATE,
            Some(&local_tag),
        )))
        .await
        .unwrap();
    let plain_update = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(plain_update.status, 200);
    assert!(plain_update.data.headers.get("Session-Expires").is_none());
    assert!(service.call.as_ref().unwrap().session_timer.is_none());
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn incoming_invite_requiring_timer_without_interval_is_rejected_before_ringing() {
    let (mut service, media, mut wire, task) = fixture();
    let mut invite = inbound("bad-session-timer", 11, Method::INVITE, None);
    push(&mut invite.data.headers, "Require", "timer").unwrap();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 400);
    assert!(wire.try_recv().is_err());
    assert!(service.call.is_none());
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn incoming_invite_below_minimum_returns_422_with_min_se_before_ringing() {
    let (mut service, media, mut wire, task) = fixture();
    let mut invite = inbound("short-session-timer", 11, Method::INVITE, None);
    push(&mut invite.data.headers, "Supported", "timer").unwrap();
    push(&mut invite.data.headers, "Session-Expires", "60").unwrap();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    let rejected = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(rejected.status, 422);
    assert_eq!(single(&rejected.data.headers, "Min-SE").unwrap(), "90");
    assert!(wire.try_recv().is_err());
    assert!(service.call.is_none());
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn observed_ussd_rejection_survives_unconfirmed_ack_without_replaying_either_request() {
    let (mut service, media, mut wire, actor) = fixture_with_ack_failure(true);
    let pending = service.start_ussd(g(), "*100#").await.unwrap();
    let invite = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(invite.method, Method::INVITE);
    let write_error = service
        .handle_event(&response_event(&invite, 403, vec![], None))
        .await
        .unwrap_err();
    assert_eq!(write_error.code, "ims_call_write_unconfirmed");
    assert!(write_error.uncertain);
    let ack = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(ack.method, Method::ACK);
    assert_eq!(
        ack.data.headers.get("Call-ID").unwrap().value(),
        invite.data.headers.get("Call-ID").unwrap().value()
    );
    let rejection = tokio::time::timeout(Duration::from_millis(100), pending.receiver)
        .await
        .expect("observed 403 must complete the Pending waiter even if ACK is unknown")
        .unwrap()
        .err()
        .expect("403 must remain a rejection");
    assert_eq!(rejection.code, "ims_ussd_rejected");
    assert_eq!(rejection.sip_status, Some(403));
    assert!(
        !rejection.uncertain,
        "carrier rejection is known, independently of ACK delivery"
    );
    service
        .tick(g(), Instant::now() + Duration::from_secs(121))
        .await
        .unwrap();
    service.shutdown(g()).await.unwrap();
    assert!(
        wire.try_recv().is_err(),
        "retirement must not replay INVITE/ACK or fabricate BYE for the rejected dialog"
    );
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    drop(service);
    actor.await.unwrap();
}
#[tokio::test]
async fn outgoing_cancel_200_race_acks_before_bye_and_never_starts_media() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let invite = wire.recv().await.unwrap().request.unwrap();
    service.hangup(g()).await.unwrap();
    let cancel = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(cancel.method, Method::CANCEL);
    assert_eq!(
        cancel.data.headers.get("Via"),
        invite.data.headers.get("Via")
    );
    assert_eq!(cancel.data.headers.get("To"), invite.data.headers.get("To"));
    for (status, rseq) in [(180, None), (183, Some(10))] {
        service
            .handle_event(&response_event(&invite, status, vec![], rseq))
            .await
            .unwrap();
        assert_eq!(service.snapshot.state, "cancelling");
        if rseq.is_some() {
            assert_eq!(
                wire.recv().await.unwrap().request.unwrap().method,
                Method::PRACK
            );
        }
        assert_eq!(media.started.load(Ordering::SeqCst), 0);
    }
    service
        .handle_event(&response_event(&invite, 200, offer(), None))
        .await
        .unwrap();
    let ack = wire.recv().await.unwrap().request.unwrap();
    let bye = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(ack.method, Method::ACK);
    assert_eq!(bye.method, Method::BYE);
    assert_eq!(ack.uri.to_string(), "sip:b@peer.ims.test");
    assert_eq!(
        ack.data.headers.get("Route").unwrap().value(),
        "<sip:edge-b.ims.test;lr>"
    );
    assert_eq!(service.snapshot.state, "ended");
    assert_eq!(media.started.load(Ordering::SeqCst), 0);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service
        .handle_event(&response_event(&invite, 200, offer(), None))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}
#[tokio::test]
async fn incoming_cancel_before_answer_returns_200_and_487_on_original_connection() {
    let (mut service, media, mut wire, task) = fixture();
    let invite = inbound("incoming-cancel", 12, Method::INVITE, None);
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 180);
    service
        .handle_event(&request_event(inbound(
            "incoming-cancel",
            12,
            Method::CANCEL,
            None,
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 487);
    assert!(service.answer(g()).await.is_err());
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    assert_eq!(service.snapshot.state, "ended");
    drop(service);
    task.await.unwrap();
}
#[tokio::test]
async fn incoming_answer_retransmits_rx_only_then_ack_enables_tx_and_late_cancel_is_a_noop() {
    let (mut service, media, mut wire, task) = fixture();
    let invite = inbound("incoming-answer", 14, Method::INVITE, None);
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    wire.recv().await.unwrap();
    let ringing = wire.recv().await.unwrap().response.unwrap();
    let localtag = tag(ringing.data.headers.get("To").unwrap().value())
        .unwrap()
        .to_owned();
    service.answer(g()).await.unwrap();
    let answer = wire.recv().await.unwrap().response.unwrap();
    assert!(!answer.data.body.is_empty());
    assert_eq!(service.snapshot.state, "connecting");
    assert_eq!(media.prepared.lock().unwrap().len(), 1);
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    assert_eq!(media.started.load(Ordering::SeqCst), 1);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert!(service.snapshot.media_ready);
    assert!(service.snapshot.early_media);
    assert!(service.snapshot.started_at.is_empty());
    assert_eq!(
        *media.events.lock().unwrap(),
        vec![
            "response100",
            "response180",
            "prepare",
            "response200",
            "configure"
        ]
    );
    assert!(service.dtmf(g(), '#').await.is_err());
    assert!(media.digits.lock().unwrap().is_empty());
    let pending = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap();
    let (retry_at, ack_deadline) = (pending.next_response_retry, pending.deadline);
    for _ in 0..2 {
        service
            .handle_event(&request_event(invite.clone()))
            .await
            .unwrap();
        assert!(
            wire.try_recv().is_err(),
            "Accepted must absorb duplicate initial INVITEs"
        );
        assert_eq!(service.snapshot.state, "connecting");
        assert!(service.snapshot.started_at.is_empty());
        assert_eq!(media.prepared.lock().unwrap().len(), 1);
        assert_eq!(media.configured.lock().unwrap().len(), 1);
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
        let pending = service
            .call
            .as_ref()
            .unwrap()
            .initial_answer
            .as_ref()
            .unwrap();
        assert_eq!(pending.next_response_retry, retry_at);
        assert_eq!(pending.deadline, ack_deadline);
    }
    service.tick(g(), retry_at).await.unwrap();
    let duplicate = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(
        proletarius_sip::Message::Response(duplicate.clone()).encode(),
        proletarius_sip::Message::Response(answer.clone()).encode()
    );
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .initial_answer
            .as_ref()
            .unwrap()
            .deadline,
        ack_deadline
    );
    assert_eq!(answer.data.body, duplicate.data.body);
    assert_eq!(
        answer.data.headers.get("To"),
        duplicate.data.headers.get("To")
    );
    let cancel = inbound("incoming-answer", 14, Method::CANCEL, None);
    let mut wrong_connection = request_event(cancel.clone());
    let RegisteredSipEvent::Message { connection, .. } = &mut wrong_connection.event else {
        unreachable!()
    };
    *connection = SipConnectionId::Incoming(99);
    service.handle_event(&wrong_connection).await.unwrap();
    let rejected = wire.recv().await.unwrap();
    assert_eq!(rejected.connection, SipConnectionId::Incoming(99));
    assert_eq!(rejected.response.unwrap().status, 481);
    service.handle_event(&request_event(cancel)).await.unwrap();
    let accepted = wire.recv().await.unwrap();
    assert_eq!(accepted.connection, SipConnectionId::Primary);
    assert_eq!(accepted.response.unwrap().status, 200);
    assert!(
        wire.try_recv().is_err(),
        "matched late CANCEL must not emit487 or terminate an accepted dialog"
    );
    assert_eq!(service.snapshot.state, "connecting");
    assert_eq!(media.prepared.lock().unwrap().len(), 1);
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    service
        .handle_event(&request_event(inbound(
            "incoming-answer",
            14,
            Method::ACK,
            Some(&localtag),
        )))
        .await
        .unwrap();
    assert_eq!(media.started.load(Ordering::SeqCst), 2);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true, false]);
    assert_eq!(media.prepared.lock().unwrap().len(), 1);
    assert_eq!(
        media.configured.lock().unwrap().len(),
        1,
        "ACK only enables TX; it must not configure again"
    );
    assert_eq!(service.snapshot.state, "active");
    assert!(!service.snapshot.early_media);
    assert!(!service.snapshot.started_at.is_empty());
    service.dtmf(g(), '#').await.unwrap();
    assert_eq!(*media.digits.lock().unwrap(), vec!['#']);
    service.hangup(g()).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    service
        .handle_event(&request_event(inbound(
            "incoming-answer",
            14,
            Method::ACK,
            Some(&localtag),
        )))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "ended");
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true, false]);
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(
        wire.try_recv().is_err(),
        "late ACK must neither restart media nor resend BYE"
    );
    drop(service);
    task.await.unwrap();
}
#[tokio::test]
async fn unanswered_incoming_expires_at_90s_and_stale_generation_never_writes() {
    let (mut service, _, mut wire, task) = fixture();
    service
        .handle_event(&request_event(inbound("timeout", 20, Method::INVITE, None)))
        .await
        .unwrap();
    wire.recv().await.unwrap();
    wire.recv().await.unwrap();
    let deadline = service.next_deadline().unwrap();
    service
        .tick(g(), deadline - Duration::from_millis(1))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    service.tick(g(), deadline).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 480);
    assert_eq!(service.snapshot.state, "ended");
    let stale = SessionGeneration::from_value(8).unwrap();
    assert!(service.dial(stale, "+447700900123").await.is_err());
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}
#[test]
fn incoming_peer_preserves_reader_number_contract() {
    for (from, peer) in [
        (
            "\"Caller\" <sip:+447700900456@ims.test;user=phone>;tag=remote",
            "+447700900456",
        ),
        (
            "<tel:+447700900456;phone-context=ims.test>;tag=remote",
            "+447700900456",
        ),
        ("sip:anonymous@anonymous.invalid;tag=remote", "anonymous"),
        ("<sip:>;tag=remote", "<sip:>;tag=remote"),
    ] {
        assert_eq!(incoming_peer(from), peer);
    }
}
#[tokio::test]
async fn bodyless_provisional_rings_locally_without_starting_media() {
    for (status, rseq) in [(180, None), (183, None), (180, Some(10)), (183, Some(10))] {
        let (mut service, media, mut wire, task) = fixture();
        service.dial(g(), "+447700900123").await.unwrap();
        let invite = wire.recv().await.unwrap().request.unwrap();
        service
            .handle_event(&response_event(&invite, status, vec![], rseq))
            .await
            .unwrap();
        assert_eq!(service.snapshot.state, "early");
        assert!(!service.snapshot.media_ready && !service.snapshot.early_media);
        assert_ne!(service.snapshot.audio_port, 0);
        assert!(!service.snapshot.audio_token.is_empty());
        assert_eq!(media.started.load(Ordering::SeqCst), 0);
        if rseq.is_some() {
            assert_eq!(
                wire.recv().await.unwrap().request.unwrap().method,
                Method::PRACK
            );
        }
        service.shutdown(g()).await.unwrap();
        drop(service);
        task.await.unwrap();
    }
}
#[tokio::test]
async fn reliable_early_answer_pracks_once_and_bodyless_final_reuses_sdp() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let invite = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_event(&invite, 183, offer(), Some(10)))
        .await
        .unwrap();
    let prack = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(prack.method, Method::PRACK);
    assert_eq!(
        prack.data.headers.get("RAck").unwrap().value(),
        "10 1 INVITE"
    );
    assert_eq!(media.started.load(Ordering::SeqCst), 1);
    service
        .handle_event(&response_event(&invite, 183, offer(), Some(10)))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    service
        .handle_event(&response_event(&invite, 200, vec![], None))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert_eq!(service.snapshot.state, "active");
    assert!(!service.snapshot.early_media);
    assert_eq!(
        media.started.load(Ordering::SeqCst),
        2,
        "final answer activates speech after early receive-only media"
    );
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}
#[tokio::test]
async fn established_update_answers_sdp_and_moves_existing_media_endpoint() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let invite = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_event(&invite, 200, offer(), None))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert_eq!(service.snapshot.state, "active");
    let call_id = single(&invite.data.headers, "Call-ID").unwrap().to_owned();
    let local_tag = tag(single(&invite.data.headers, "From").unwrap()).unwrap();
    let mut update = inbound(&call_id, 2, Method::UPDATE, Some(local_tag));
    update
        .data
        .headers
        .set(Header::new("From", "<sip:b@ims.test>;tag=remote-tag").unwrap());
    update.data.body = String::from_utf8(offer())
        .unwrap()
        .replace("m=audio 49172", "m=audio 49174")
        .into_bytes();
    push(&mut update.data.headers, "Content-Type", "application/sdp").unwrap();
    service.handle_event(&request_event(update)).await.unwrap();
    let response = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        response.data.headers.get("Content-Type").unwrap().value(),
        "application/sdp"
    );
    let answer = String::from_utf8(response.data.body).unwrap();
    assert!(answer.contains("o=- 7 2 IN IP4 192.0.2.10"));
    assert!(answer.contains("m=audio 49170 RTP/AVP 96"));
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    assert_eq!(
        media.configured.lock().unwrap().last().unwrap().remote_port,
        49174
    );
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}
#[tokio::test]
async fn established_reinvite_switches_media_after_answer_and_commits_sip_once_on_ack() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_event(&initial, 200, offer(), None))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    let call_id = single(&initial.data.headers, "Call-ID").unwrap().to_owned();
    let local_tag = tag(single(&initial.data.headers, "From").unwrap()).unwrap();
    let mut invite = in_dialog(&call_id, 2, Method::INVITE, local_tag);
    invite.data.body = String::from_utf8(offer())
        .unwrap()
        .replace("m=audio 49172", "m=audio 49174")
        .into_bytes();
    invite
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@new-peer.ims.test>").unwrap());
    push(&mut invite.data.headers, "Content-Type", "application/sdp").unwrap();
    let initial_media = media.configured.lock().unwrap().clone();
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    let accepted = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(accepted.status, 200);
    assert!(
        String::from_utf8(accepted.data.body.clone())
            .unwrap()
            .contains("o=- 7 2 IN IP4 192.0.2.10")
    );
    assert_eq!(
        media.configured.lock().unwrap().len(),
        initial_media.len() + 1
    );
    assert_eq!(
        media.configured.lock().unwrap().last().unwrap().remote_port,
        49174
    );
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 1);
    let prepares = media.prepared.lock().unwrap().len();
    let retained = service.call.as_ref().unwrap().remote.clone();
    let transaction = service
        .call
        .as_ref()
        .unwrap()
        .accepted_reinvites
        .back()
        .unwrap();
    let (retry_at, ack_deadline, retry_interval) = (
        transaction.next_response_retry.unwrap(),
        transaction.deadline,
        transaction.response_retry_interval,
    );
    assert_eq!(transaction.cseq, 2);
    assert!(transaction.confirmed);
    assert!(!transaction.acked);
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    assert!(
        wire.try_recv().is_err(),
        "Accepted absorbs duplicate INVITE"
    );
    let transaction = service
        .call
        .as_ref()
        .unwrap()
        .accepted_reinvites
        .back()
        .unwrap();
    assert_eq!(transaction.next_response_retry, Some(retry_at));
    assert_eq!(transaction.deadline, ack_deadline);
    assert_eq!(transaction.response_retry_interval, retry_interval);
    assert!(!transaction.acked);
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .pending_reinvite
            .as_ref()
            .unwrap()
            .deadline,
        ack_deadline
    );
    assert_eq!(service.call.as_ref().unwrap().remote, retained);
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 1);
    assert_eq!(media.prepared.lock().unwrap().len(), prepares);
    assert_eq!(
        media.configured.lock().unwrap().len(),
        initial_media.len() + 1
    );
    service.tick(g(), retry_at).await.unwrap();
    let repeated = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(repeated, accepted, "TU retries the complete original 200");
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .accepted_reinvites
            .back()
            .unwrap()
            .deadline,
        ack_deadline
    );
    service
        .handle_event(&request_event(in_dialog(
            &call_id,
            3,
            Method::INVITE,
            local_tag,
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 491);
    assert_eq!(
        media.configured.lock().unwrap().len(),
        initial_media.len() + 1
    );
    assert_eq!(
        media.configured.lock().unwrap().last().unwrap().remote_port,
        49174
    );

    service
        .handle_event(&request_event(in_dialog(
            &call_id,
            2,
            Method::ACK,
            local_tag,
        )))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 2);
    assert_eq!(
        media.configured.lock().unwrap().len(),
        initial_media.len() + 1
    );
    assert_eq!(
        media.configured.lock().unwrap().last().unwrap().remote_port,
        49174
    );
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .dialog
            .as_ref()
            .unwrap()
            .remote_target
            .to_string(),
        "sip:b@new-peer.ims.test"
    );
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(
        wire.try_recv().is_err(),
        "ACKed Accepted absorbs old INVITE"
    );
    let transaction = service
        .call
        .as_ref()
        .unwrap()
        .accepted_reinvites
        .back()
        .unwrap();
    assert!(transaction.acked);
    assert_eq!(transaction.next_response_retry, None);
    assert_eq!(transaction.deadline, ack_deadline);
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 2);
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
    assert_eq!(media.prepared.lock().unwrap().len(), prepares);
    assert_eq!(
        media.configured.lock().unwrap().len(),
        initial_media.len() + 1
    );
    assert_eq!(
        media.configured.lock().unwrap().last().unwrap().remote_port,
        49174
    );
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn unsupported_reinvite_keeps_established_media_and_ack_timeout_drains() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_event(&initial, 200, offer(), None))
        .await
        .unwrap();
    wire.recv().await.unwrap(); // Initial ACK.
    let call_id = single(&initial.data.headers, "Call-ID").unwrap().to_owned();
    let local_tag = tag(single(&initial.data.headers, "From").unwrap()).unwrap();
    let initial_media = media.configured.lock().unwrap().clone();
    let mut invalid = in_dialog(&call_id, 2, Method::INVITE, local_tag);
    invalid.data.body =
        b"v=0\r\nc=IN IP4 192.0.2.20\r\nm=audio 49172 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n"
            .to_vec();
    push(&mut invalid.data.headers, "Content-Type", "application/sdp").unwrap();
    service
        .handle_event(&request_event(invalid.clone()))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 488);
    service.handle_event(&request_event(invalid)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 488);
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(*media.configured.lock().unwrap(), initial_media);

    let mut supported = in_dialog(&call_id, 3, Method::INVITE, local_tag);
    push(
        &mut supported.data.headers,
        "Content-Type",
        "application/sdp",
    )
    .unwrap();
    service
        .handle_event(&request_event(supported))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    let deadline = service
        .call
        .as_ref()
        .unwrap()
        .pending_reinvite
        .as_ref()
        .unwrap()
        .deadline;
    service.tick(g(), deadline).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_reinvite_ack_timeout");
    assert_eq!(*media.configured.lock().unwrap(), initial_media);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn rejected_reinvite_retransmits_the_same_415_and_400_without_ending_call() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_event(&initial, 200, offer(), None))
        .await
        .unwrap();
    wire.recv().await.unwrap(); // Initial ACK.
    let call_id = single(&initial.data.headers, "Call-ID").unwrap().to_owned();
    let local_tag = tag(single(&initial.data.headers, "From").unwrap()).unwrap();
    let original_media = media.configured.lock().unwrap().clone();

    let unsupported_type = in_dialog(&call_id, 2, Method::INVITE, local_tag);
    for _ in 0..2 {
        service
            .handle_event(&request_event(unsupported_type.clone()))
            .await
            .unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 415);
    }

    let mut malformed_contact = in_dialog(&call_id, 3, Method::INVITE, local_tag);
    push(
        &mut malformed_contact.data.headers,
        "Content-Type",
        "application/sdp",
    )
    .unwrap();
    malformed_contact
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@peer.ims.test").unwrap());
    for _ in 0..2 {
        service
            .handle_event(&request_event(malformed_contact.clone()))
            .await
            .unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 400);
    }
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(*media.configured.lock().unwrap(), original_media);
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn offerless_reinvite_sends_offer_and_uses_ack_answer_for_media() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_event(&initial, 200, offer(), None))
        .await
        .unwrap();
    wire.recv().await.unwrap(); // Initial ACK.
    let call_id = single(&initial.data.headers, "Call-ID").unwrap().to_owned();
    let local_tag = tag(single(&initial.data.headers, "From").unwrap());
    let local_tag = local_tag.unwrap();
    let current = service.call.as_ref().unwrap().remote.clone().unwrap();
    let original_media = media.configured.lock().unwrap().clone();
    let mut refresh = in_dialog(&call_id, 2, Method::INVITE, local_tag);
    refresh.data.body.clear();
    service.handle_event(&request_event(refresh)).await.unwrap();
    let response = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(response.status, 200);
    assert!(
        String::from_utf8(response.data.body)
            .unwrap()
            .contains("o=- 7 2 IN IP4 192.0.2.10")
    );
    assert_eq!(*media.configured.lock().unwrap(), original_media);

    let mut ack = in_dialog(&call_id, 2, Method::ACK, local_tag);
    ack.data.body = sdp::build_local_sdp(
        9,
        "192.0.2.20".parse().unwrap(),
        49178,
        true,
        Some(current.codec),
        Some(current.payload_type),
        current.telephone_event_payload,
        false,
    )
    .unwrap()
    .into_bytes();
    push(&mut ack.data.headers, "Content-Type", "application/sdp").unwrap();
    service.handle_event(&request_event(ack)).await.unwrap();
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 2);
    assert_eq!(
        media.configured.lock().unwrap().last().unwrap().remote_port,
        49178
    );
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn offerless_reinvite_without_ack_answer_ends_dialog_and_media() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_event(&initial, 200, offer(), None))
        .await
        .unwrap();
    wire.recv().await.unwrap(); // Initial ACK.
    let call_id = single(&initial.data.headers, "Call-ID").unwrap().to_owned();
    let local_tag = tag(single(&initial.data.headers, "From").unwrap()).unwrap();
    let original_media = media.configured.lock().unwrap().clone();
    let mut refresh = in_dialog(&call_id, 2, Method::INVITE, local_tag);
    refresh.data.body.clear();
    service.handle_event(&request_event(refresh)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    service
        .handle_event(&request_event(in_dialog(
            &call_id,
            2,
            Method::ACK,
            local_tag,
        )))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_reinvite_ack_sdp_invalid");
    assert_eq!(*media.configured.lock().unwrap(), original_media);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn answered_incoming_dialog_accepts_supported_reinvite_and_rejects_hold_offer() {
    let (mut service, media, mut wire, task) = fixture();
    let call_id = "incoming-reinvite";
    let mut invite = inbound(call_id, 14, Method::INVITE, None);
    push(
        &mut invite.data.headers,
        "Session-Expires",
        "180;refresher=uac",
    )
    .unwrap();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 180);
    service.answer(g()).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    let local_tag = service.call.as_ref().unwrap().local_tag.clone();
    service
        .handle_event(&request_event(inbound(
            call_id,
            14,
            Method::ACK,
            Some(&local_tag),
        )))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "active");
    assert!(service.call.as_ref().unwrap().session_timer.is_some());
    let original = media.configured.lock().unwrap().clone();

    let mut held = inbound(call_id, 15, Method::INVITE, Some(&local_tag));
    held.data.body.extend_from_slice(b"a=inactive\r\n");
    push(&mut held.data.headers, "Content-Type", "application/sdp").unwrap();
    service.handle_event(&request_event(held)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 488);
    assert_eq!(*media.configured.lock().unwrap(), original);
    assert_eq!(service.snapshot.state, "active");

    let mut too_short = inbound(call_id, 16, Method::INVITE, Some(&local_tag));
    push(
        &mut too_short.data.headers,
        "Content-Type",
        "application/sdp",
    )
    .unwrap();
    push(
        &mut too_short.data.headers,
        "Session-Expires",
        "180;refresher=uac",
    )
    .unwrap();
    push(&mut too_short.data.headers, "Min-SE", "240").unwrap();
    for _ in 0..2 {
        service
            .handle_event(&request_event(too_short.clone()))
            .await
            .unwrap();
        let response = wire.recv().await.unwrap().response.unwrap();
        assert_eq!(response.status, 422);
        assert_eq!(single(&response.data.headers, "Min-SE").unwrap(), "240");
    }
    assert!(service.call.as_ref().unwrap().session_timer.is_some());

    let mut refresh = inbound(call_id, 17, Method::INVITE, Some(&local_tag));
    refresh.data.body = String::from_utf8(offer())
        .unwrap()
        .replace("m=audio 49172", "m=audio 49176")
        .into_bytes();
    push(&mut refresh.data.headers, "Content-Type", "application/sdp").unwrap();
    service.handle_event(&request_event(refresh)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    assert_eq!(media.configured.lock().unwrap().len(), original.len() + 1);
    assert_eq!(
        media.configured.lock().unwrap().last().unwrap().remote_port,
        49176
    );
    service
        .handle_event(&request_event(inbound(
            call_id,
            17,
            Method::ACK,
            Some(&local_tag),
        )))
        .await
        .unwrap();
    assert_eq!(
        media.configured.lock().unwrap().last().unwrap().remote_port,
        49176
    );
    assert_eq!(service.snapshot.state, "active");
    assert!(service.call.as_ref().unwrap().session_timer.is_none());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}
#[tokio::test]
async fn unsupported_update_offer_is_rejected_without_changing_media_or_sdp_version() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let invite = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_event(&invite, 200, offer(), None))
        .await
        .unwrap();
    wire.recv().await.unwrap(); // Initial ACK.
    let call_id = single(&invite.data.headers, "Call-ID").unwrap().to_owned();
    let local_tag = tag(single(&invite.data.headers, "From").unwrap()).unwrap();
    let make_update = |seq, body: Vec<u8>, content_type| {
        let mut request = inbound(&call_id, seq, Method::UPDATE, Some(local_tag));
        request
            .data
            .headers
            .set(Header::new("From", "<sip:b@ims.test>;tag=remote-tag").unwrap());
        request.data.body = body;
        push(&mut request.data.headers, "Content-Type", content_type).unwrap();
        request
    };
    let original = media.configured.lock().unwrap().clone();
    service
        .handle_event(&request_event(make_update(2, offer(), "text/plain")))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 415);
    let pcma = b"v=0\r\nc=IN IP4 192.0.2.20\r\nm=audio 49172 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n"
        .to_vec();
    service
        .handle_event(&request_event(make_update(3, pcma, "application/sdp")))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 488);
    assert_eq!(*media.configured.lock().unwrap(), original);
    service
        .handle_event(&request_event(make_update(4, offer(), "application/sdp")))
        .await
        .unwrap();
    let accepted = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(accepted.status, 200);
    assert!(
        String::from_utf8(accepted.data.body)
            .unwrap()
            .contains("o=- 7 2 IN IP4 192.0.2.10")
    );
    assert_eq!(service.snapshot.state, "active");
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}
#[tokio::test]
async fn shutdown_waits_for_actual_media_drain_without_extra_signaling() {
    let (mut service, media, mut wire, task) = fixture();
    service.dial(g(), "+447700900123").await.unwrap();
    let invite = wire.recv().await.unwrap().request.unwrap();
    service
        .handle_event(&response_event(&invite, 200, offer(), None))
        .await
        .unwrap();
    wire.recv().await.unwrap();
    let hold = Arc::new(tokio::sync::Notify::new());
    *media.drain.lock().unwrap() = Some(hold.clone());
    let mut closing = tokio::spawn(async move {
        service.shutdown(g()).await.unwrap();
        service
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(15), &mut closing)
            .await
            .is_err()
    );
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    assert!(wire.try_recv().is_err());
    hold.notify_one();
    let service = closing.await.unwrap();
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}
#[tokio::test]
async fn disabled_codecs_never_ring_or_allocate_media() {
    let (mut service, media, mut wire, task) = fixture();
    let mut invite = inbound("nb-disabled", 20, Method::INVITE, None);
    invite.data.body=b"v=0\r\nc=IN IP4 192.0.2.1\r\nm=audio 49170 RTP/AVP 8 100\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:100 AMR/8000\r\n".to_vec();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 488);
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    assert_eq!(service.snapshot.state, "idle");
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn ussd_signaling_dialog_queues_pre200_info_without_allocating_voice_and_continues_once() {
    let (mut service, media, mut wire, task) = fixture();
    let pending = service.start_ussd(g(), "*100#").await.unwrap();
    assert_eq!(service.snapshot().state, "idle");
    assert!(
        service.active(),
        "live USSD must defer REGISTER renewal even with no voice dialog"
    );
    let invite = wire.recv().await.unwrap().request.unwrap();
    assert!(String::from_utf8_lossy(&invite.data.body).contains("m=audio 0"));
    assert!(
        invite
            .data
            .headers
            .get("Content-Type")
            .unwrap()
            .value()
            .starts_with("multipart/mixed")
    );
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    let mut info = inbound(
        &pending.session_id,
        2,
        Method::INFO,
        Some(tag(invite.data.headers.get("From").unwrap().value()).unwrap()),
    );
    info.data
        .headers
        .set(Header::new("From", "<sip:b@ims.test>;tag=remote-tag").unwrap());
    push(&mut info.data.headers, "Info-Package", "g.3gpp.ussd").unwrap();
    push(
        &mut info.data.headers,
        "Content-Type",
        "application/vnd.3gpp.ussd+xml",
    )
    .unwrap();
    info.data.body = proletarius_ims::ussd_xml::encode("Balance?").unwrap();
    service
        .handle_event(&request_event(info.clone()))
        .await
        .unwrap();
    assert!(
        wire.try_recv().is_err(),
        "USSD owns bounded pre-final INFO, no premature voice response"
    );
    service
        .handle_event(&response_event(&invite, 200, vec![], None))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    let result = pending.receiver.await.unwrap().unwrap();
    assert_eq!(result.text, "Balance?");
    assert!(result.needs_input);
    assert!(!result.complete);
    let continuation = service
        .continue_ussd(g(), &result.session_id, "1")
        .await
        .unwrap();
    let request = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(request.method, Method::INFO);
    assert_eq!(
        request.data.headers.get("Info-Package").unwrap().value(),
        "g.3gpp.ussd"
    );
    assert!(
        service
            .continue_ussd(g(), &result.session_id, "1")
            .await
            .is_err()
    );
    info.data
        .headers
        .set(Header::new("CSeq", "3 INFO").unwrap());
    info.data.body = proletarius_ims::ussd_xml::encode("Done").unwrap();
    service.handle_event(&request_event(info)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    assert_eq!(continuation.receiver.await.unwrap().unwrap().text, "Done");
    service.cancel_ussd(g(), &result.session_id).await.unwrap();
    assert!(
        !service.active(),
        "confirmed USSD termination must release renewal admission"
    );
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    drop(service);
    task.await.unwrap();
}
#[tokio::test]
async fn ussd_timeout_200_race_acks_and_byes_original_transaction_without_voice() {
    let (mut service, media, mut wire, task) = fixture();
    let pending = service.start_ussd(g(), "*100#").await.unwrap();
    let invite = wire.recv().await.unwrap().request.unwrap();
    let deadline = service.next_deadline().unwrap();
    service.tick(g(), deadline).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::CANCEL
    );
    assert!(pending.receiver.await.unwrap().is_err());
    service
        .handle_event(&response_event(&invite, 200, vec![], None))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    service
        .handle_event(&response_event(&invite, 200, vec![], None))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert!(wire.try_recv().is_err());
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    drop(service);
    task.await.unwrap();
}

include!("renegotiation_tests.rs");
include!("initial_answer_tests.rs");
include!("accepted_reinvite_tests.rs");
include!("initial_offerless_tests.rs");
include!("reliable_provisional_tests.rs");
include!("completed_invite_tests.rs");
