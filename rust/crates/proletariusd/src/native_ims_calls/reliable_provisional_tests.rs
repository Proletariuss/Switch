// UAS 100rel coverage uses the production owner and a writer with independent
// receipts for reliable180, PRACK200 and final INVITE200. No network/device IO.
macro_rules! rp_recv {
    ($wire:expr) => {
        tokio::time::timeout(Duration::from_secs(2), $wire.recv())
            .await
            .expect("reliable provisional wire/receipt timeout")
            .expect("reliable provisional writer closed")
    };
}
#[derive(Default)]
struct RpWriter {
    hold_ringing: AtomicBool,
    hold_prack: AtomicBool,
    hold_prack_final: AtomicBool,
    hold_final: AtomicBool,
    hold_bye: AtomicBool,
    hold_cancel: AtomicBool,
    hold_update: AtomicBool,
    hold_rejection: AtomicBool,
}
struct RpReceipt {
    status: u16,
    method: Method,
    reply: oneshot::Sender<std::io::Result<()>>,
}
fn rp_fixture() -> (
    Service,
    Arc<FakeMedia>,
    Arc<RpWriter>,
    mpsc::UnboundedReceiver<Wire>,
    mpsc::UnboundedReceiver<RpReceipt>,
    tokio::task::JoinHandle<()>,
) {
    let (commands, mut input) = mpsc::channel(32);
    let (sent, output) = mpsc::unbounded_channel();
    let (receipts, controlled) = mpsc::unbounded_channel();
    let media = Arc::new(FakeMedia::default());
    let writer_media = media.clone();
    let controls = Arc::new(RpWriter::default());
    let writer = controls.clone();
    let task = tokio::spawn(async move {
        while let Some(command) = input.recv().await {
            match command {
                Command::SendUntracked {
                    generation,
                    request,
                    reply,
                } => {
                    assert_eq!(generation, g());
                    sent.send(Wire {
                        request: Some(request),
                        response: None,
                        connection: SipConnectionId::Primary,
                    })
                    .unwrap();
                    let _ = reply.send(Ok(()));
                }
                Command::RespondDialog {
                    generation,
                    connection,
                    response,
                    reply,
                } => {
                    assert_eq!(generation, g());
                    let method = cseq(&response.data.headers).unwrap().1;
                    let status = response.status;
                    let hold = (status == 180 && writer.hold_ringing.load(Ordering::SeqCst))
                        || (status == 200
                            && method == Method::PRACK
                            && writer.hold_prack.load(Ordering::SeqCst))
                        || (status >= 200
                            && method == Method::PRACK
                            && writer.hold_prack_final.load(Ordering::SeqCst))
                        || (status == 200
                            && method == Method::INVITE
                            && writer.hold_final.load(Ordering::SeqCst))
                        || (status == 200
                            && method == Method::BYE
                            && writer.hold_bye.load(Ordering::SeqCst))
                        || (status == 200
                            && method == Method::CANCEL
                            && writer.hold_cancel.load(Ordering::SeqCst))
                        || (status == 200
                            && method == Method::UPDATE
                            && writer.hold_update.load(Ordering::SeqCst))
                        || (status >= 300
                            && method == Method::INVITE
                            && writer.hold_rejection.load(Ordering::SeqCst));
                    writer_media
                        .events
                        .lock()
                        .unwrap()
                        .push(format!("response{status}"));
                    sent.send(Wire {
                        request: None,
                        response: Some(response),
                        connection,
                    })
                    .unwrap();
                    if hold {
                        receipts
                            .send(RpReceipt {
                                status,
                                method,
                                reply,
                            })
                            .unwrap();
                    } else {
                        let _ = reply.send(Ok(()));
                    }
                }
                _ => panic!("100rel must not borrow a tracked SMS transaction"),
            }
        }
    });
    let service = Service::new(g(), config(), commands, media.clone()).unwrap();
    (service, media, controls, output, controlled, task)
}
fn rp_invite(call_id: &str, offerless: bool) -> Request {
    let mut invite = inbound(call_id, 14, Method::INVITE, None);
    if offerless {
        invite.data.body.clear();
    }
    push(&mut invite.data.headers, "Require", "100rel").unwrap();
    invite
}
fn rp_request(call_id: &str, local_tag: &str, seq: u32, method: Method) -> Request {
    let mut request = inbound(call_id, seq, method, Some(local_tag));
    request.data.headers.set(
        Header::new(
            "Via",
            format!("SIP/2.0/TCP peer.ims.test;branch=z9hG4bKrp-{seq};rport"),
        )
        .unwrap(),
    );
    request
}
fn rp_prack(call_id: &str, response: &Response, seq: u32) -> Request {
    let local_tag = tag(single(&response.data.headers, "To").unwrap()).unwrap();
    let mut request = rp_request(call_id, local_tag, seq, Method::PRACK);
    push(
        &mut request.data.headers,
        "RAck",
        &format!(
            "{} 14 INVITE",
            single(&response.data.headers, "RSeq").unwrap()
        ),
    )
    .unwrap();
    request
}
fn rp_answer(response: &Response, evs: bool) -> Vec<u8> {
    let offer = std::str::from_utf8(&response.data.body).unwrap();
    let (_, modes) = proletarius_call::evs::available_audio_codecs(false);
    let selected = sdp::negotiate_audio_offer_with_modes(offer, evs, &modes).unwrap();
    assert_eq!(matches!(selected.codec, AudioCodec::Evs { .. }), evs);
    sdp::build_local_audio_answer_to_offer_version(
        570,
        1,
        "192.0.2.20".parse().unwrap(),
        49200,
        offer,
        &selected,
        false,
    )
    .unwrap()
    .into_bytes()
}
fn rp_set_sdp(request: &mut Request, body: Vec<u8>) {
    request.data.body = body;
    push(&mut request.data.headers, "Content-Type", "application/sdp").unwrap();
}
fn rp_rx_only(media: &FakeMedia, snapshot: &Snapshot) {
    assert!(media.start_modes.lock().unwrap().iter().all(|early| *early));
    assert!(media.digits.lock().unwrap().is_empty());
    assert_ne!(snapshot.state, "active");
    assert!(snapshot.started_at.is_empty());
}

async fn rp_complete_exchange(offerless: bool, prack_new_offer: bool) {
    let (mut service, media, controls, mut wire, mut receipts, task) = rp_fixture();
    controls.hold_ringing.store(true, Ordering::SeqCst);
    controls.hold_prack.store(true, Ordering::SeqCst);
    controls.hold_final.store(true, Ordering::SeqCst);
    let call_id = format!("rp-flow-{offerless}-{prack_new_offer}");
    let invite = rp_invite(&call_id, offerless);
    let event = request_event(invite.clone());
    let mut incoming = Box::pin(service.handle_event(&event));
    let trying = tokio::select! {
        result = &mut incoming => panic!("incoming finished before reliable180 receipt: {result:?}"),
        message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
    };
    assert_eq!(trying.status, 100);
    let ringing = tokio::select! {
        result = &mut incoming => panic!("incoming finished before reliable180 receipt: {result:?}"),
        message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
    };
    assert_eq!(ringing.status, 180);
    assert!(token(&ringing.data.headers, "Require", "100rel"));
    let rseq = single(&ringing.data.headers, "RSeq")
        .unwrap()
        .parse::<u32>()
        .unwrap();
    assert!(rseq > 0);
    assert!(rseq <= i32::MAX as u32);
    assert!(!ringing.data.body.is_empty());
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    assert!(media.configured.lock().unwrap().is_empty());
    assert!(media.start_modes.lock().unwrap().is_empty());
    if offerless {
        assert_eq!(
            *media.local_offers.lock().unwrap(),
            vec![ringing.data.body.clone()]
        );
    } else {
        assert_eq!(media.prepared.lock().unwrap().len(), 1);
    }
    let receipt = rp_recv!(receipts);
    assert_eq!((receipt.status, receipt.method), (180, Method::INVITE));
    receipt.reply.send(Ok(())).unwrap();
    incoming.await.unwrap();
    rp_rx_only(&media, &service.snapshot);
    if !offerless {
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    }
    let local_tag = tag(single(&ringing.data.headers, "To").unwrap())
        .unwrap()
        .to_owned();

    // A user's answer intent cannot outrun the reliable O/A exchange or its
    // PRACK200 receipt, including when the reliable180 already answered SDP.
    tokio::time::timeout(Duration::from_secs(2), service.answer(g()))
        .await
        .unwrap()
        .unwrap();
    assert!(wire.try_recv().is_err());
    assert!(service.call.as_ref().unwrap().answer_requested);
    rp_rx_only(&media, &service.snapshot);
    let mut prack = rp_prack(&call_id, &ringing, 15);
    if offerless {
        rp_set_sdp(&mut prack, rp_answer(&ringing, false));
    } else if prack_new_offer {
        rp_set_sdp(
            &mut prack,
            p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
        );
    }
    let event = request_event(prack.clone());
    let mut handling = Box::pin(service.handle_event(&event));
    let acknowledged = tokio::select! {
        result = &mut handling => panic!("PRACK completed before controlled receipt: {result:?}"),
        message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
    };
    assert_eq!(acknowledged.status, 200);
    assert_eq!(
        cseq(&acknowledged.data.headers).unwrap(),
        (15, Method::PRACK)
    );
    assert_eq!(!acknowledged.data.body.is_empty(), prack_new_offer);
    assert_eq!(
        media.configured.lock().unwrap().len(),
        if offerless { 0 } else { 1 },
        "new media cannot commit before the PRACK200 receipt"
    );
    assert!(
        wire.try_recv().is_err(),
        "final200 must wait for PRACK200 receipt"
    );
    let receipt = rp_recv!(receipts);
    assert_eq!((receipt.status, receipt.method), (200, Method::PRACK));
    receipt.reply.send(Ok(())).unwrap();
    let final_response = tokio::select! {
        result = &mut handling => panic!("deferred answer completed before final200 receipt: {result:?}"),
        message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
    };
    assert_eq!(final_response.status, 200);
    assert_eq!(
        cseq(&final_response.data.headers).unwrap(),
        (14, Method::INVITE)
    );
    assert!(
        final_response.data.body.is_empty(),
        "reliable O/A is complete; final200 must be bodyless"
    );
    assert!(final_response.data.headers.get("RSeq").is_none());
    assert!(media.start_modes.lock().unwrap().iter().all(|early| *early));
    let receipt = rp_recv!(receipts);
    assert_eq!((receipt.status, receipt.method), (200, Method::INVITE));
    receipt.reply.send(Ok(())).unwrap();
    handling.await.unwrap();
    rp_rx_only(&media, &service.snapshot);
    assert_eq!(service.snapshot.state, "connecting");
    assert!(service.snapshot.media_ready);
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().len();
    assert_eq!(configured.len(), if prack_new_offer { 2 } else { 1 });
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    controls.hold_prack.store(false, Ordering::SeqCst);
    service.handle_event(&request_event(prack)).await.unwrap();
    assert_eq!(
        proletarius_sip::Message::Response(rp_recv!(wire).response.unwrap()).encode(),
        proletarius_sip::Message::Response(acknowledged).encode()
    );
    assert_eq!(*media.configured.lock().unwrap(), configured);
    let ack = rp_request(&call_id, &local_tag, 14, Method::ACK);
    service
        .handle_event(&request_event(ack.clone()))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true, false]);
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    service.handle_event(&request_event(ack)).await.unwrap();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true, false]);
    service.shutdown(g()).await.unwrap();
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}
#[tokio::test]
async fn reliable_uas_offered_invite_empty_prack_bodyless_final_and_ack() {
    rp_complete_exchange(false, false).await;
}
#[tokio::test]
async fn reliable_uas_offered_invite_prack_new_offer_is_answered_before_bodyless_final() {
    rp_complete_exchange(false, true).await;
}
#[tokio::test]
async fn reliable_uas_offerless_invite_prack_answer_bodyless_final_and_ack() {
    rp_complete_exchange(true, false).await;
}

async fn rp_ring(
    service: &mut Service,
    wire: &mut mpsc::UnboundedReceiver<Wire>,
    invite: &Request,
) -> Response {
    tokio::time::timeout(
        Duration::from_secs(2),
        service.handle_event(&request_event(invite.clone())),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 100);
    let response = rp_recv!(wire).response.unwrap();
    assert_eq!(response.status, 180);
    assert!(token(&response.data.headers, "Require", "100rel"));
    response
}
fn rp_write_failure() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "controlled reliable UAS writer failure",
    )
}
#[tokio::test]
async fn reliable_uas_timer_doubles_without_t2_cap_and_freezes_complete_180_bytes() {
    let (mut service, media, mut wire, task) = fixture();
    let invite = rp_invite("rp-timers", false);
    let response = rp_ring(&mut service, &mut wire, &invite).await;
    let bytes = proletarius_sip::Message::Response(response).encode();
    let reliable = service.call.as_ref().unwrap().reliable.as_ref().unwrap();
    let deadline = reliable.deadline;
    let retry_at = reliable.next_response_retry.unwrap();
    assert_eq!(reliable.response_retry_interval, Duration::from_millis(500));
    assert!(deadline >= retry_at + Duration::from_millis(31_500));
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().len();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(
        proletarius_sip::Message::Response(rp_recv!(wire).response.unwrap()).encode(),
        bytes
    );
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .reliable
            .as_ref()
            .unwrap()
            .next_response_retry,
        Some(retry_at)
    );
    for seconds in [1, 2, 4, 8, 16, 32] {
        let at = service
            .call
            .as_ref()
            .unwrap()
            .reliable
            .as_ref()
            .unwrap()
            .next_response_retry
            .unwrap();
        assert!(at < deadline);
        service.tick(g(), at).await.unwrap();
        assert_eq!(
            proletarius_sip::Message::Response(rp_recv!(wire).response.unwrap()).encode(),
            bytes
        );
        let reliable = service.call.as_ref().unwrap().reliable.as_ref().unwrap();
        assert_eq!(reliable.deadline, deadline);
        assert_eq!(
            reliable.response_retry_interval,
            Duration::from_secs(seconds)
        );
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(media.prepared.lock().unwrap().len(), prepared);
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    }
    service.tick(g(), deadline).await.unwrap();
    let timed_out = rp_recv!(wire).response.unwrap();
    assert_eq!(timed_out.status, 504);
    assert_eq!(cseq(&timed_out.data.headers).unwrap(), (14, Method::INVITE));
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_reliable_prack_timeout");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    service
        .tick(g(), deadline + Duration::from_secs(90))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn reliable_uas_rack_validation_connection_dialog_and_prack_transaction_identity_are_strict()
{
    let (mut service, media, mut wire, task) = fixture();
    let invite = rp_invite("rp-validation", false);
    let response = rp_ring(&mut service, &mut wire, &invite).await;
    let baseline = media.configured.lock().unwrap().clone();
    let reliable = service.call.as_ref().unwrap().reliable.as_ref().unwrap();
    let (deadline, retry_at) = (reliable.deadline, reliable.next_response_retry);
    let rseq = single(&response.data.headers, "RSeq").unwrap();
    for (index, rack) in [
        None,
        Some("".to_owned()),
        Some(format!("{rseq} 14")),
        Some(format!("{rseq} 14 INVITE extra")),
        Some("0 14 INVITE".into()),
        Some("4294967296 14 INVITE".into()),
        Some(format!("{rseq} 14 invite")),
        Some(format!("{rseq} 14 ACK")),
    ]
    .into_iter()
    .enumerate()
    {
        let local_tag = tag(single(&response.data.headers, "To").unwrap()).unwrap();
        let mut request = rp_request(
            "rp-validation",
            local_tag,
            100 + index as u32,
            Method::PRACK,
        );
        if let Some(rack) = rack {
            push(&mut request.data.headers, "RAck", &rack).unwrap();
        }
        service.handle_event(&request_event(request)).await.unwrap();
        assert_eq!(rp_recv!(wire).response.unwrap().status, 400);
    }
    for (index, mismatch) in ["rseq", "invite-cseq", "connection", "from", "to"]
        .into_iter()
        .enumerate()
    {
        let mut request = rp_prack("rp-validation", &response, 200 + index as u32);
        match mismatch {
            "rseq" => {
                request.data.headers.set(
                    Header::new(
                        "RAck",
                        format!("{} 14 INVITE", rseq.parse::<u32>().unwrap() + 1),
                    )
                    .unwrap(),
                );
            }
            "invite-cseq" => {
                request
                    .data
                    .headers
                    .set(Header::new("RAck", format!("{rseq} 13 INVITE")).unwrap());
            }
            "from" => {
                request
                    .data
                    .headers
                    .set(Header::new("From", "<sip:b@ims.test>;tag=other").unwrap());
            }
            "to" => {
                request
                    .data
                    .headers
                    .set(Header::new("To", "<sip:a@ims.test>;tag=other").unwrap());
            }
            _ => {}
        }
        let mut event = request_event(request);
        if mismatch == "connection" {
            let RegisteredSipEvent::Message { connection, .. } = &mut event.event else {
                unreachable!()
            };
            *connection = SipConnectionId::Incoming(99);
        }
        service.handle_event(&event).await.unwrap();
        assert_eq!(rp_recv!(wire).response.unwrap().status, 481);
    }
    let mut duplicate_rack = rp_prack("rp-validation", &response, 300);
    push(
        &mut duplicate_rack.data.headers,
        "RAck",
        &format!("{rseq} 14 INVITE"),
    )
    .unwrap();
    service
        .handle_event(&request_event(duplicate_rack))
        .await
        .unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 400);
    let reliable = service.call.as_ref().unwrap().reliable.as_ref().unwrap();
    assert_eq!(
        (
            reliable.deadline,
            reliable.next_response_retry,
            reliable.pracked
        ),
        (deadline, retry_at, false)
    );
    assert_eq!(*media.configured.lock().unwrap(), baseline);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    let prack = rp_prack("rp-validation", &response, 15);
    service
        .handle_event(&request_event(prack.clone()))
        .await
        .unwrap();
    let accepted = rp_recv!(wire).response.unwrap();
    assert_eq!(
        accepted.status, 200,
        "rejected CSeq99 attempts must not advance the early dialog"
    );
    let reliable = service.call.as_ref().unwrap().reliable.as_ref().unwrap();
    assert!(reliable.pracked);
    assert!(reliable.next_response_retry.is_none());
    // Transaction matching freezes the first outcome even if a retransmitter
    // changes its RAck/body. They cannot turn this duplicate into a new offer.
    let mut duplicate = prack.clone();
    duplicate
        .data
        .headers
        .set(Header::new("RAck", "999 14 INVITE").unwrap());
    rp_set_sdp(
        &mut duplicate,
        p5_wb_offer(111, 49202, "octet-align=1;mode-set=2"),
    );
    service
        .handle_event(&request_event(duplicate))
        .await
        .unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap(), accepted);
    let mut changed_via = prack.clone();
    changed_via.data.headers.set(
        Header::new(
            "Via",
            "SIP/2.0/TCP peer.ims.test;branch=z9hG4bKnew-transaction;rport",
        )
        .unwrap(),
    );
    service
        .handle_event(&request_event(changed_via))
        .await
        .unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 481);
    let mut changed_cseq = prack;
    changed_cseq
        .data
        .headers
        .set(Header::new("CSeq", "16 PRACK").unwrap());
    service
        .handle_event(&request_event(changed_cseq))
        .await
        .unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 481);
    assert_eq!(*media.configured.lock().unwrap(), baseline);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    service.tick(g(), deadline).await.unwrap();
    assert!(
        wire.try_recv().is_err(),
        "PRACK confirmation stops reliable180 retries and timeout"
    );
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn reliable_uas_early_bodyless_update_contact_cseq_and_timer_survive_final_and_ack() {
    let (mut service, media, mut wire, task) = fixture();
    let call_id = "rp-early-update";
    let mut invite = rp_invite(call_id, false);
    push(
        &mut invite.data.headers,
        "Session-Expires",
        "180;refresher=uac",
    )
    .unwrap();
    let response = rp_ring(&mut service, &mut wire, &invite).await;
    let local_tag = tag(single(&response.data.headers, "To").unwrap()).unwrap();
    service.answer(g()).await.unwrap();
    assert!(wire.try_recv().is_err());
    let mut update = rp_request(call_id, local_tag, 16, Method::UPDATE);
    update
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@latest-early-target.ims.test>").unwrap());
    push(
        &mut update.data.headers,
        "Session-Expires",
        "300;refresher=uac",
    )
    .unwrap();
    service.handle_event(&request_event(update)).await.unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
    let retained_timer = p5_session_timer_facts(&service);
    assert_eq!(retained_timer.unwrap().0, 300);
    let configured = media.configured.lock().unwrap().clone();
    let prack = rp_prack(call_id, &response, 17);
    service.handle_event(&request_event(prack)).await.unwrap();
    let prack_response = rp_recv!(wire).response.unwrap();
    assert_eq!(
        cseq(&prack_response.data.headers).unwrap(),
        (17, Method::PRACK)
    );
    let final_response = rp_recv!(wire).response.unwrap();
    assert_eq!(final_response.status, 200);
    assert!(final_response.data.body.is_empty());
    assert_eq!(
        single(&final_response.data.headers, "Session-Expires").unwrap(),
        "300;refresher=uac"
    );
    assert_eq!(p5_session_timer_facts(&service), retained_timer);
    service
        .handle_event(&request_event(rp_request(
            call_id,
            local_tag,
            14,
            Method::ACK,
        )))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(p5_session_timer_facts(&service), retained_timer);
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
        "sip:b@latest-early-target.ims.test"
    );
    assert_eq!(*media.configured.lock().unwrap(), configured);
    // A new lower-CSeq UPDATE cannot undo either accepted early transaction.
    let mut stale = rp_request(call_id, local_tag, 16, Method::UPDATE);
    stale.data.headers.set(
        Header::new(
            "Via",
            "SIP/2.0/TCP peer.ims.test;branch=z9hG4bKrp-stale-update;rport",
        )
        .unwrap(),
    );
    stale
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@stale.ims.test>").unwrap());
    service.handle_event(&request_event(stale)).await.unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 500);
    assert_eq!(p5_session_timer_facts(&service), retained_timer);
    service.hangup(g()).await.unwrap();
    let bye = rp_recv!(wire).request.unwrap();
    assert_eq!(bye.method, Method::BYE);
    assert_eq!(bye.uri.to_string(), "sip:b@latest-early-target.ims.test");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn reliable_uas_cancel_hangup_and_timeout_stop_rx_without_early_bye_or_late_prack_restart() {
    for ending in ["cancel", "hangup", "timeout"] {
        let (mut service, media, mut wire, task) = fixture();
        let call_id = format!("rp-end-{ending}");
        let invite = rp_invite(&call_id, false);
        let ringing = rp_ring(&mut service, &mut wire, &invite).await;
        let deadline = service
            .call
            .as_ref()
            .unwrap()
            .reliable
            .as_ref()
            .unwrap()
            .deadline;
        match ending {
            "cancel" => {
                service
                    .handle_event(&request_event(inbound(&call_id, 14, Method::CANCEL, None)))
                    .await
                    .unwrap();
                let cancel = rp_recv!(wire).response.unwrap();
                assert_eq!(cancel.status, 200);
                assert_eq!(cseq(&cancel.data.headers).unwrap().1, Method::CANCEL);
            }
            "hangup" => {
                service.hangup(g()).await.unwrap();
            }
            _ => {
                service.tick(g(), deadline).await.unwrap();
            }
        }
        let final_response = rp_recv!(wire).response.unwrap();
        assert_eq!(
            final_response.status,
            match ending {
                "cancel" => 487,
                "hangup" => 486,
                _ => 504,
            }
        );
        assert_eq!(
            cseq(&final_response.data.headers).unwrap(),
            (14, Method::INVITE)
        );
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
        assert!(!service.snapshot.media_ready);
        let prepared = media.prepared.lock().unwrap().len();
        let configured = media.configured.lock().unwrap().clone();
        let late = rp_prack(&call_id, &ringing, 15);
        service
            .handle_event(&request_event(late.clone()))
            .await
            .unwrap();
        let acknowledged = rp_recv!(wire).response.unwrap();
        assert_eq!(acknowledged.status, 200);
        assert!(acknowledged.data.body.is_empty());
        service.handle_event(&request_event(late)).await.unwrap();
        assert_eq!(rp_recv!(wire).response.unwrap(), acknowledged);
        assert_eq!(media.prepared.lock().unwrap().len(), prepared);
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
        service
            .tick(g(), deadline + Duration::from_secs(90))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn reliable_uas_retired_late_prack_and_exact_cache_replay_cannot_touch_new_active_media() {
    let (mut service, media, mut wire, task) = fixture();
    let old = rp_invite("rp-retired", false);
    let response = rp_ring(&mut service, &mut wire, &old).await;
    service
        .handle_event(&request_event(inbound(
            "rp-retired",
            14,
            Method::CANCEL,
            None,
        )))
        .await
        .unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
    assert_eq!(rp_recv!(wire).response.unwrap().status, 487);
    let (new_call_id, _) = p5_establish(&mut service, &mut wire).await;
    assert_ne!(new_call_id, "rp-retired");
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().len();
    let start_modes = media.start_modes.lock().unwrap().clone();
    let token = service.snapshot.audio_token.clone();
    let target = service
        .call
        .as_ref()
        .unwrap()
        .dialog
        .as_ref()
        .unwrap()
        .remote_target
        .clone();
    let late = rp_prack("rp-retired", &response, 15);
    service
        .handle_event(&request_event(late.clone()))
        .await
        .unwrap();
    let acknowledged = rp_recv!(wire).response.unwrap();
    assert_eq!(acknowledged.status, 200);
    assert_eq!(
        single(&acknowledged.data.headers, "Call-ID").unwrap(),
        "rp-retired"
    );
    service.handle_event(&request_event(late)).await.unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap(), acknowledged);
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.snapshot.call_id, new_call_id);
    assert_eq!(service.snapshot.audio_token, token);
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .dialog
            .as_ref()
            .unwrap()
            .remote_target,
        target
    );
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(*media.start_modes.lock().unwrap(), start_modes);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn reliable_uas_unknown_180_prack200_and_final200_are_fenced_and_not_replayed_as_known() {
    for phase in ["180", "prack200", "final200"] {
        let (mut service, media, controls, mut wire, mut receipts, task) = rp_fixture();
        let call_id = format!("rp-unknown-{phase}");
        let invite = rp_invite(&call_id, false);
        let ringing;
        if phase == "180" {
            controls.hold_ringing.store(true, Ordering::SeqCst);
            let event = request_event(invite.clone());
            let mut incoming = Box::pin(service.handle_event(&event));
            let trying = tokio::select! {
                result = &mut incoming => panic!("180 finished without receipt {result:?}"),
                message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
            };
            assert_eq!(trying.status, 100);
            ringing = tokio::select! {
                result = &mut incoming => panic!("180 finished without receipt {result:?}"),
                message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
            };
            rp_recv!(receipts)
                .reply
                .send(Err(rp_write_failure()))
                .unwrap();
            assert!(incoming.await.unwrap_err().uncertain);
            let rejected = rp_recv!(wire).response.unwrap();
            assert_eq!(rejected.status, 500);
            service
                .handle_event(&request_event(invite.clone()))
                .await
                .unwrap();
            assert_eq!(
                rp_recv!(wire).response.unwrap(),
                rejected,
                "replay the known final500, never the unknown reliable180"
            );
        } else {
            ringing = rp_ring(&mut service, &mut wire, &invite).await;
            let prack = rp_prack(&call_id, &ringing, 15);
            if phase == "prack200" {
                controls.hold_prack.store(true, Ordering::SeqCst);
                let event = request_event(prack.clone());
                let mut handling = Box::pin(service.handle_event(&event));
                let accepted = tokio::select! {
                    result = &mut handling => panic!("PRACK200 finished without receipt {result:?}"),
                    message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
                };
                assert_eq!(accepted.status, 200);
                rp_recv!(receipts)
                    .reply
                    .send(Err(rp_write_failure()))
                    .unwrap();
                assert!(handling.await.unwrap_err().uncertain);
                assert_eq!(rp_recv!(wire).response.unwrap().status, 500);
                service.handle_event(&request_event(prack)).await.unwrap();
                assert!(
                    wire.try_recv().is_err(),
                    "unknown PRACK200 is an absorbing tombstone"
                );
            } else {
                service.handle_event(&request_event(prack)).await.unwrap();
                assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
                controls.hold_final.store(true, Ordering::SeqCst);
                let mut answering = Box::pin(service.answer(g()));
                let final_response = tokio::select! {
                    result = &mut answering => panic!("final200 finished without receipt; error={:?}", result.err()),
                    message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
                };
                assert!(final_response.data.body.is_empty());
                rp_recv!(receipts)
                    .reply
                    .send(Err(rp_write_failure()))
                    .unwrap();
                assert!(answering.await.err().unwrap().uncertain);
                assert!(wire.try_recv().is_err());
                let local_tag = tag(single(&ringing.data.headers, "To").unwrap()).unwrap();
                let ack = rp_request(&call_id, local_tag, 14, Method::ACK);
                service
                    .handle_event(&request_event(ack.clone()))
                    .await
                    .unwrap();
                let bye = rp_recv!(wire).request.unwrap();
                assert_eq!(bye.method, Method::BYE);
                assert_eq!(single(&bye.data.headers, "Call-ID").unwrap(), call_id);
                service.handle_event(&request_event(ack)).await.unwrap();
                assert!(wire.try_recv().is_err());
            }
        }
        assert_eq!(service.snapshot.state, "failed");
        assert!(service.snapshot.uncertain);
        assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        assert!(media.start_modes.lock().unwrap().iter().all(|early| *early));
        assert!(wire.try_recv().is_err());
        service.shutdown(g()).await.unwrap();
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn reliable_uas_allocation_prepare_configure_and_start_failures_reject_invite_and_drain() {
    for fault in ["allocate", "prepare", "local-offer", "configure", "start"] {
        let (mut service, media, mut wire, task) = fixture();
        match fault {
            "allocate" => media.allocate_fail.store(true, Ordering::SeqCst),
            "prepare" => media.prepare_fail.store(true, Ordering::SeqCst),
            "local-offer" => media.prepare_local_offer_fail.store(true, Ordering::SeqCst),
            "configure" => media.configure_fail.store(true, Ordering::SeqCst),
            "start" => media.start_fail.store(true, Ordering::SeqCst),
            _ => unreachable!(),
        }
        let event = request_event(rp_invite(
            &format!("rp-media-{fault}"),
            fault == "local-offer",
        ));
        assert!(
            tokio::time::timeout(Duration::from_secs(2), service.handle_event(&event))
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(rp_recv!(wire).response.unwrap().status, 100);
        if matches!(fault, "configure" | "start") {
            let reliable = rp_recv!(wire).response.unwrap();
            assert_eq!(reliable.status, 180);
            assert!(token(&reliable.data.headers, "Require", "100rel"));
        }
        let rejected = rp_recv!(wire).response.unwrap();
        assert_eq!(
            rejected.status,
            if matches!(fault, "prepare" | "local-offer") {
                488
            } else {
                500
            }
        );
        assert_eq!(cseq(&rejected.data.headers).unwrap(), (14, Method::INVITE));
        assert_eq!(service.snapshot.state, "failed");
        assert!(!service.snapshot.media_ready);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        assert_eq!(
            media.stopped.load(Ordering::SeqCst),
            if fault == "allocate" { 0 } else { 1 }
        );
        assert!(media.start_modes.lock().unwrap().iter().all(|early| *early));
        assert!(
            wire.try_recv().is_err(),
            "a rejected early INVITE must not send BYE"
        );
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn reliable_uas_invalid_prack_sdp_answers_prack_transaction_then_rejects_early_invite() {
    for fault in [
        "missing-answer",
        "outside-offer",
        "unsupported-new-offer",
        "prepare-new-offer",
    ] {
        let (mut service, media, mut wire, task) = fixture();
        let offerless = matches!(fault, "missing-answer" | "outside-offer");
        let call_id = format!("rp-bad-sdp-{fault}");
        let response = rp_ring(&mut service, &mut wire, &rp_invite(&call_id, offerless)).await;
        let mut prack = rp_prack(&call_id, &response, 15);
        match fault {
            "outside-offer" => rp_set_sdp(&mut prack, p5_wb_offer(111, 49200, "octet-align=1;mode-set=2")),
            "unsupported-new-offer" => rp_set_sdp(&mut prack, b"v=0\r\no=- 99 1 IN IP4 192.0.2.20\r\ns=peer\r\nc=IN IP4 192.0.2.20\r\nt=0 0\r\nm=audio 49200 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n".to_vec()),
            "prepare-new-offer" => {
                media.prepare_fail.store(true, Ordering::SeqCst);
                rp_set_sdp(&mut prack, p5_wb_offer(111, 49200, "octet-align=1;mode-set=2"));
            }
            _ => {}
        }
        let before = media.configured.lock().unwrap().clone();
        assert!(
            service
                .handle_event(&request_event(prack.clone()))
                .await
                .is_err()
        );
        let acknowledged = rp_recv!(wire).response.unwrap();
        assert_eq!(acknowledged.status, 200);
        assert_eq!(
            cseq(&acknowledged.data.headers).unwrap(),
            (15, Method::PRACK)
        );
        if fault.ends_with("new-offer") {
            let answer = std::str::from_utf8(&acknowledged.data.body).unwrap();
            assert!(answer.contains("m=audio 0 RTP/AVP"));
            assert!(answer.contains("a=inactive"));
        } else {
            assert!(acknowledged.data.body.is_empty());
        }
        let rejected = rp_recv!(wire).response.unwrap();
        assert_eq!(
            rejected.status,
            if fault == "prepare-new-offer" {
                500
            } else {
                488
            }
        );
        assert_eq!(cseq(&rejected.data.headers).unwrap(), (14, Method::INVITE));
        assert_eq!(*media.configured.lock().unwrap(), before);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        service.handle_event(&request_event(prack)).await.unwrap();
        assert_eq!(rp_recv!(wire).response.unwrap(), acknowledged);
        assert!(wire.try_recv().is_err());
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn reliable_uas_failed_stop_keeps_resource_fence_and_late_prack_does_not_retry_or_start_media()
 {
    let (mut service, media, mut wire, task) = fixture();
    let response = rp_ring(&mut service, &mut wire, &rp_invite("rp-stop-fence", false)).await;
    media.stop_fail.store(true, Ordering::SeqCst);
    let error = service
        .handle_event(&request_event(inbound(
            "rp-stop-fence",
            14,
            Method::CANCEL,
            None,
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code, "ims_media_stop_failed");
    assert!(error.uncertain);
    assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
    assert_eq!(rp_recv!(wire).response.unwrap().status, 487);
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert!(service.call.as_ref().unwrap().media_allocated);
    assert!(service.active());
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    assert!(service.dial(g(), "+447700900124").await.is_err());
    let configured = media.configured.lock().unwrap().clone();
    let prack = rp_prack("rp-stop-fence", &response, 15);
    service.handle_event(&request_event(prack)).await.unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    assert!(wire.try_recv().is_err());
    media.stop_fail.store(false, Ordering::SeqCst);
    service.shutdown(g()).await.unwrap();
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 2);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn reliable_uas_terminal_codec_report_sends_early_final_and_never_bye_or_transmit_start() {
    let (mut service, media, mut wire, task) = fixture();
    rp_ring(
        &mut service,
        &mut wire,
        &rp_invite("rp-terminal-media", false),
    )
    .await;
    *media.terminal_error.lock().unwrap() = Some(Failure::known("ims_media_decode_failed"));
    service.tick(g(), Instant::now()).await.unwrap();
    let rejected = rp_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 500);
    assert_eq!(service.snapshot.last_error, "ims_media_decode_failed");
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn reliable_uas_sixteen_retired_records_protect_identity_until_their_bounded_grace_expires() {
    let (mut service, media, mut wire, task) = fixture();
    let mut first = None;
    let mut last_deadline = Instant::now();
    for index in 0..=16 {
        let call_id = format!("rp-cap-{index}");
        let response = rp_ring(&mut service, &mut wire, &rp_invite(&call_id, false)).await;
        last_deadline = service
            .call
            .as_ref()
            .unwrap()
            .reliable
            .as_ref()
            .unwrap()
            .retain_until;
        if first.is_none() {
            first = Some(response);
        }
        service
            .handle_event(&request_event(inbound(&call_id, 14, Method::CANCEL, None)))
            .await
            .unwrap();
        assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
        assert_eq!(rp_recv!(wire).response.unwrap().status, 487);
    }
    assert_eq!(service.retired.len(), 16);
    assert_eq!(media.allocated.load(Ordering::SeqCst), 17);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 17);
    let blocked = rp_invite("rp-cap-blocked", false);
    service.handle_event(&request_event(blocked)).await.unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 486);
    assert_eq!(media.allocated.load(Ordering::SeqCst), 17);
    let first = first.unwrap();
    service
        .handle_event(&request_event(rp_prack("rp-cap-0", &first, 15)))
        .await
        .unwrap();
    let late = rp_recv!(wire).response.unwrap();
    assert_eq!(late.status, 200);
    assert_eq!(single(&late.data.headers, "Call-ID").unwrap(), "rp-cap-0");
    assert_eq!(media.allocated.load(Ordering::SeqCst), 17);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 17);
    service
        .tick(g(), last_deadline + Duration::from_secs(1))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    rp_ring(
        &mut service,
        &mut wire,
        &rp_invite("rp-cap-after-expiry", false),
    )
    .await;
    assert_eq!(media.allocated.load(Ordering::SeqCst), 18);
    assert!(service.retired.len() <= 16);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn reliable_uas_remote_early_bye_finishes_invite_and_drains_despite_either_writer_failure() {
    for failure in ["none", "bye200", "invite487"] {
        let (mut service, media, controls, mut wire, mut receipts, task) = rp_fixture();
        let call_id = format!("rp-early-bye-{failure}");
        let ringing = rp_ring(&mut service, &mut wire, &rp_invite(&call_id, false)).await;
        let local_tag = tag(single(&ringing.data.headers, "To").unwrap()).unwrap();
        controls.hold_bye.store(true, Ordering::SeqCst);
        controls.hold_rejection.store(true, Ordering::SeqCst);
        let bye = request_event(rp_request(&call_id, local_tag, 15, Method::BYE));
        let mut ending = Box::pin(service.handle_event(&bye));
        let acknowledged = tokio::select! {
            result = &mut ending => panic!("early BYE completed without controlled receipt {result:?}"),
            message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
        };
        assert_eq!(
            (
                acknowledged.status,
                cseq(&acknowledged.data.headers).unwrap().1
            ),
            (200, Method::BYE)
        );
        let receipt = rp_recv!(receipts);
        assert_eq!((receipt.status, receipt.method), (200, Method::BYE));
        receipt
            .reply
            .send(if failure == "bye200" {
                Err(rp_write_failure())
            } else {
                Ok(())
            })
            .unwrap();
        let rejected = tokio::select! {
            result = &mut ending => panic!("early BYE failed to finish its original INVITE {result:?}"),
            message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
        };
        assert_eq!(
            (rejected.status, cseq(&rejected.data.headers).unwrap().1),
            (487, Method::INVITE)
        );
        let receipt = rp_recv!(receipts);
        assert_eq!((receipt.status, receipt.method), (487, Method::INVITE));
        receipt
            .reply
            .send(if failure == "invite487" {
                Err(rp_write_failure())
            } else {
                Ok(())
            })
            .unwrap();
        let result = ending.await;
        assert_eq!(result.is_err(), failure != "none");
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        assert!(!service.snapshot.media_ready);
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
        let configured = media.configured.lock().unwrap().clone();
        let prepared = media.prepared.lock().unwrap().len();
        let late = rp_prack(&call_id, &ringing, 16);
        service.handle_event(&request_event(late)).await.unwrap();
        let prack = rp_recv!(wire).response.unwrap();
        assert_eq!(
            (prack.status, cseq(&prack.data.headers).unwrap().1),
            (200, Method::PRACK)
        );
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(media.prepared.lock().unwrap().len(), prepared);
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(
            wire.try_recv().is_err(),
            "remote early BYE must never generate an outgoing BYE"
        );
        service.shutdown(g()).await.unwrap();
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn reliable_uas_unknown_early_update_finishes_invite_but_after_final_preserves_accepted_cleanup()
 {
    for final_sent in [false, true] {
        let (mut service, media, controls, mut wire, mut receipts, task) = rp_fixture();
        let call_id = format!("rp-update-unknown-{final_sent}");
        let ringing = rp_ring(&mut service, &mut wire, &rp_invite(&call_id, false)).await;
        let local_tag = tag(single(&ringing.data.headers, "To").unwrap()).unwrap();
        if final_sent {
            service
                .handle_event(&request_event(rp_prack(&call_id, &ringing, 15)))
                .await
                .unwrap();
            assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
            service.answer(g()).await.unwrap();
            let final_response = rp_recv!(wire).response.unwrap();
            assert_eq!(final_response.status, 200);
            assert!(final_response.data.body.is_empty());
        }
        controls.hold_update.store(true, Ordering::SeqCst);
        let mut update = rp_request(&call_id, local_tag, 16, Method::UPDATE);
        update
            .data
            .headers
            .set(Header::new("Contact", "<sip:b@unconfirmed-early-update.ims.test>").unwrap());
        push(
            &mut update.data.headers,
            "Session-Expires",
            "300;refresher=uac",
        )
        .unwrap();
        let event = request_event(update);
        let mut updating = Box::pin(service.handle_event(&event));
        let response = tokio::select! {
            result = &mut updating => panic!("UPDATE completed before controlled receipt {result:?}"),
            message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
        };
        assert_eq!(
            (response.status, cseq(&response.data.headers).unwrap().1),
            (200, Method::UPDATE)
        );
        rp_recv!(receipts)
            .reply
            .send(Err(rp_write_failure()))
            .unwrap();
        assert!(updating.await.unwrap_err().uncertain);
        assert_eq!(service.snapshot.state, "failed");
        assert!(service.snapshot.uncertain);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        let configured = media.configured.lock().unwrap().clone();
        if !final_sent {
            let rejected = rp_recv!(wire).response.unwrap();
            assert_eq!(
                (rejected.status, cseq(&rejected.data.headers).unwrap().1),
                (500, Method::INVITE)
            );
            service
                .handle_event(&request_event(rp_prack(&call_id, &ringing, 17)))
                .await
                .unwrap();
            assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
        } else {
            assert!(
                wire.try_recv().is_err(),
                "a known final200 cannot be replaced by an INVITE500"
            );
            let ack = rp_request(&call_id, local_tag, 14, Method::ACK);
            service
                .handle_event(&request_event(ack.clone()))
                .await
                .unwrap();
            let bye = rp_recv!(wire).request.unwrap();
            assert_eq!(bye.method, Method::BYE);
            assert_eq!(single(&bye.data.headers, "Call-ID").unwrap(), call_id);
            service.handle_event(&request_event(ack)).await.unwrap();
        }
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
        assert!(wire.try_recv().is_err());
        service.shutdown(g()).await.unwrap();
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn plain_supported_only_early_update_survives_initial_answer_and_ack_without_100rel() {
    let (mut service, media, mut wire, task) = fixture();
    let call_id = "plain-early-update";
    let mut invite = inbound(call_id, 14, Method::INVITE, None);
    push(&mut invite.data.headers, "Supported", "100rel,timer").unwrap();
    push(
        &mut invite.data.headers,
        "Session-Expires",
        "180;refresher=uac",
    )
    .unwrap();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 100);
    let ringing = rp_recv!(wire).response.unwrap();
    assert_eq!(ringing.status, 180);
    assert!(ringing.data.body.is_empty());
    assert!(ringing.data.headers.get("RSeq").is_none());
    let local_tag = tag(single(&ringing.data.headers, "To").unwrap()).unwrap();
    let mut update = rp_request(call_id, local_tag, 15, Method::UPDATE);
    update
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@plain-early-target.ims.test>").unwrap());
    push(
        &mut update.data.headers,
        "Session-Expires",
        "300;refresher=uac",
    )
    .unwrap();
    service.handle_event(&request_event(update)).await.unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
    let timer = p5_session_timer_facts(&service);
    assert_eq!(timer.unwrap().0, 300);
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    service.answer(g()).await.unwrap();
    let final_response = rp_recv!(wire).response.unwrap();
    assert_eq!(final_response.status, 200);
    assert!(!final_response.data.body.is_empty());
    assert_eq!(
        single(&final_response.data.headers, "Session-Expires").unwrap(),
        "300;refresher=uac"
    );
    assert_eq!(p5_session_timer_facts(&service), timer);
    service
        .handle_event(&request_event(rp_request(
            call_id,
            local_tag,
            14,
            Method::ACK,
        )))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(p5_session_timer_facts(&service), timer);
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true, false]);
    service.hangup(g()).await.unwrap();
    let bye = rp_recv!(wire).request.unwrap();
    assert_eq!(bye.uri.to_string(), "sip:b@plain-early-target.ims.test");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn reliable_uas_unknown_cancel200_still_finishes_invite487_and_stops_early_media_once() {
    let (mut service, media, controls, mut wire, mut receipts, task) = rp_fixture();
    rp_ring(
        &mut service,
        &mut wire,
        &rp_invite("rp-unknown-cancel", false),
    )
    .await;
    controls.hold_cancel.store(true, Ordering::SeqCst);
    let event = request_event(inbound("rp-unknown-cancel", 14, Method::CANCEL, None));
    let mut canceling = Box::pin(service.handle_event(&event));
    let response = tokio::select! {
        result = &mut canceling => panic!("CANCEL finished before writer receipt {result:?}"),
        message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
    };
    assert_eq!(
        (response.status, cseq(&response.data.headers).unwrap().1),
        (200, Method::CANCEL)
    );
    let receipt = rp_recv!(receipts);
    assert_eq!((receipt.status, receipt.method), (200, Method::CANCEL));
    receipt.reply.send(Err(rp_write_failure())).unwrap();
    assert!(canceling.await.unwrap_err().uncertain);
    let rejected = rp_recv!(wire).response.unwrap();
    assert_eq!(
        (rejected.status, cseq(&rejected.data.headers).unwrap().1),
        (487, Method::INVITE)
    );
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn reliable_uas_early_session_expiry_after_prack_sends_invite408_and_stops_without_bye() {
    let (mut service, media, mut wire, task) = fixture();
    let call_id = "rp-early-session-expiry";
    let ringing = rp_ring(&mut service, &mut wire, &rp_invite(call_id, false)).await;
    let local_tag = tag(single(&ringing.data.headers, "To").unwrap()).unwrap();
    service
        .handle_event(&request_event(rp_prack(call_id, &ringing, 15)))
        .await
        .unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
    let mut update = rp_request(call_id, local_tag, 16, Method::UPDATE);
    push(
        &mut update.data.headers,
        "Session-Expires",
        "90;refresher=uac",
    )
    .unwrap();
    service.handle_event(&request_event(update)).await.unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
    let at = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap()
        .termination_at;
    service.tick(g(), at).await.unwrap();
    let rejected = rp_recv!(wire).response.unwrap();
    assert_eq!(
        (rejected.status, cseq(&rejected.data.headers).unwrap().1),
        (408, Method::INVITE)
    );
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_session_expired");
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

async fn rp_first_rejected_prack_result_is_frozen(malformed: bool) {
    for confirmed in [false, true] {
        let (mut service, media, controls, mut wire, mut receipts, task) = rp_fixture();
        let call_id = format!("rp-error-final-{malformed}-{confirmed}");
        let ringing = rp_ring(&mut service, &mut wire, &rp_invite(&call_id, false)).await;
        let baseline = media.configured.lock().unwrap().clone();
        let prepared = media.prepared.lock().unwrap().len();
        let call = service.call.as_ref().unwrap();
        let remote = call.remote.clone();
        let dialog_cseq = call.dialog.as_ref().unwrap().remote_cseq();
        let pending = call.reliable.as_ref().unwrap();
        let (deadline, retry_at) = (pending.deadline, pending.next_response_retry);
        let mut first = rp_prack(&call_id, &ringing, 15);
        let rack = if malformed {
            "invalid".to_owned()
        } else {
            format!(
                "{} 14 INVITE",
                single(&ringing.data.headers, "RSeq")
                    .unwrap()
                    .parse::<u32>()
                    .unwrap()
                    + 1
            )
        };
        first.data.headers.set(Header::new("RAck", rack).unwrap());
        controls.hold_prack_final.store(true, Ordering::SeqCst);
        let event = request_event(first.clone());
        let mut rejecting = Box::pin(service.handle_event(&event));
        let rejected = tokio::select! {
            result = &mut rejecting => panic!("PRACK error final finished before receipt {result:?}"),
            message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
        };
        assert_eq!(rejected.status, if malformed { 400 } else { 481 });
        assert_eq!(cseq(&rejected.data.headers).unwrap(), (15, Method::PRACK));
        let receipt = rp_recv!(receipts);
        assert_eq!(
            (receipt.status, receipt.method),
            (rejected.status, Method::PRACK)
        );
        receipt
            .reply
            .send(if confirmed {
                Ok(())
            } else {
                Err(rp_write_failure())
            })
            .unwrap();
        let result = rejecting.await;
        if confirmed {
            result.unwrap();
        } else {
            assert!(result.unwrap_err().uncertain);
        }
        assert!(wire.try_recv().is_err());
        let call = service.call.as_ref().unwrap();
        assert_eq!(call.dialog.as_ref().unwrap().remote_cseq(), dialog_cseq);
        assert!(!call.reliable.as_ref().unwrap().pracked);
        assert_eq!(
            call.reliable.as_ref().unwrap().next_response_retry,
            retry_at
        );
        assert_eq!(call.reliable.as_ref().unwrap().deadline, deadline);
        assert_eq!(call.remote, remote);
        assert!(
            !call.uncertain,
            "an unknown PRACK rejection does not change the INVITE outcome"
        );
        assert_eq!(media.stopped.load(Ordering::SeqCst), 0);

        // Same transaction identity, now claiming the correct RAck and a new
        // WB offer. A final response owns this identity before its receipt.
        controls.hold_prack_final.store(false, Ordering::SeqCst);
        let mut corrected = first;
        corrected.data.headers.set(
            Header::new(
                "RAck",
                format!(
                    "{} 14 INVITE",
                    single(&ringing.data.headers, "RSeq").unwrap()
                ),
            )
            .unwrap(),
        );
        rp_set_sdp(
            &mut corrected,
            p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
        );
        service
            .handle_event(&request_event(corrected))
            .await
            .unwrap();
        if confirmed {
            assert_eq!(
                proletarius_sip::Message::Response(rp_recv!(wire).response.unwrap()).encode(),
                proletarius_sip::Message::Response(rejected).encode()
            );
        } else {
            assert!(
                wire.try_recv().is_err(),
                "the unknown error final is absorbed, never upgraded to PRACK200"
            );
        }
        let call = service.call.as_ref().unwrap();
        assert_eq!(call.dialog.as_ref().unwrap().remote_cseq(), dialog_cseq);
        assert!(!call.reliable.as_ref().unwrap().pracked);
        assert_eq!(call.remote, remote);
        assert_eq!(media.prepared.lock().unwrap().len(), prepared);
        assert_eq!(*media.configured.lock().unwrap(), baseline);
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
        assert_eq!(service.snapshot.codec, "EVS");

        // A genuinely new transaction can still acknowledge that RSeq and
        // submit the WB offer. Rejected requests consumed neither SDP nor CSeq.
        let mut valid = rp_prack(&call_id, &ringing, 16);
        rp_set_sdp(
            &mut valid,
            p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
        );
        service.handle_event(&request_event(valid)).await.unwrap();
        let accepted = rp_recv!(wire).response.unwrap();
        assert_eq!(accepted.status, 200);
        assert!(!accepted.data.body.is_empty());
        let call = service.call.as_ref().unwrap();
        assert!(call.reliable.as_ref().unwrap().pracked);
        assert_eq!(call.dialog.as_ref().unwrap().remote_cseq(), 16);
        assert_eq!(media.prepared.lock().unwrap().len(), prepared + 1);
        assert_eq!(media.configured.lock().unwrap().len(), baseline.len() + 1);
        assert_eq!(service.snapshot.codec, "AMR-WB");
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
        service.answer(g()).await.unwrap();
        let final_response = rp_recv!(wire).response.unwrap();
        assert_eq!(final_response.status, 200);
        assert!(final_response.data.body.is_empty());
        let local_tag = tag(single(&ringing.data.headers, "To").unwrap()).unwrap();
        service
            .handle_event(&request_event(rp_request(
                &call_id,
                local_tag,
                14,
                Method::ACK,
            )))
            .await
            .unwrap();
        assert_eq!(service.snapshot.state, "active");
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true, false]);
        assert_eq!(media.prepared.lock().unwrap().len(), prepared + 1);
        assert_eq!(media.configured.lock().unwrap().len(), baseline.len() + 1);
        assert!(wire.try_recv().is_err());
        service.shutdown(g()).await.unwrap();
        drop(service);
        task.await.unwrap();
    }
}
#[tokio::test]
async fn reliable_uas_first_prack400_known_and_unknown_freeze_transaction_before_corrected_rack_offer()
 {
    rp_first_rejected_prack_result_is_frozen(true).await;
}
#[tokio::test]
async fn reliable_uas_first_prack481_known_and_unknown_freeze_transaction_before_corrected_rack_offer()
 {
    rp_first_rejected_prack_result_is_frozen(false).await;
}

#[tokio::test]
async fn reliable_uas_invalid_prack_cseq_method_or_via_does_not_consume_reliable_exchange() {
    let (mut service, media, mut wire, task) = fixture();
    let ringing = rp_ring(&mut service, &mut wire, &rp_invite("rp-cseq-via", false)).await;
    let cseq_before = service
        .call
        .as_ref()
        .unwrap()
        .dialog
        .as_ref()
        .unwrap()
        .remote_cseq();
    for (index, malformed) in ["cseq-method", "via-branch"].into_iter().enumerate() {
        let mut request = rp_prack("rp-cseq-via", &ringing, 100 + index as u32);
        if malformed == "cseq-method" {
            request
                .data
                .headers
                .set(Header::new("CSeq", "100 INVITE").unwrap());
        } else {
            request.data.headers.set(
                Header::new("Via", "SIP/2.0/TCP peer.ims.test;branch=not-magic-cookie").unwrap(),
            );
        }
        service.handle_event(&request_event(request)).await.unwrap();
        assert_eq!(rp_recv!(wire).response.unwrap().status, 400);
        let call = service.call.as_ref().unwrap();
        assert_eq!(call.dialog.as_ref().unwrap().remote_cseq(), cseq_before);
        assert!(!call.reliable.as_ref().unwrap().pracked);
        assert_eq!(media.prepared.lock().unwrap().len(), 1);
        assert_eq!(media.configured.lock().unwrap().len(), 1);
    }
    service
        .handle_event(&request_event(rp_prack("rp-cseq-via", &ringing, 15)))
        .await
        .unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn reliable_uas_retained_rack_identity_never_replays_180_after_final_cache_expires() {
    let (mut service, media, mut wire, task) = fixture();
    let invite = rp_invite("rp-expired-final", false);
    let ringing = rp_ring(&mut service, &mut wire, &invite).await;
    service
        .handle_event(&request_event(inbound(
            "rp-expired-final",
            14,
            Method::CANCEL,
            None,
        )))
        .await
        .unwrap();
    assert_eq!(rp_recv!(wire).response.unwrap().status, 200);
    let rejected = rp_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 487);
    let call = service.call.as_ref().unwrap();
    let final_expiry = call.initial_reply_deadline.unwrap();
    let rack_expiry = call.reliable.as_ref().unwrap().retain_until;
    assert!(final_expiry < rack_expiry);
    let at = final_expiry + Duration::from_millis(1);
    assert!(at < rack_expiry);
    service.tick(g(), at).await.unwrap();
    let call = service.call.as_ref().unwrap();
    assert!(call.initial_reply.is_none());
    assert!(call.reliable.as_ref().unwrap().confirmed);
    assert!(!call.media_allocated);
    let prepared = media.prepared.lock().unwrap().len();
    let configured = media.configured.lock().unwrap().clone();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(
        wire.try_recv().is_err(),
        "retained RAck identity must not replay180 after INVITE final response expired"
    );
    service
        .handle_event(&request_event(rp_prack("rp-expired-final", &ringing, 15)))
        .await
        .unwrap();
    let late = rp_recv!(wire).response.unwrap();
    assert_eq!(late.status, 200);
    assert_eq!(cseq(&late.data.headers).unwrap().1, Method::PRACK);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn reliable_uas_fifteen_error_finals_preserve_known_unknown_results_and_reserve_valid_prack_slot()
 {
    let (mut service, media, controls, mut wire, mut receipts, task) = rp_fixture();
    let call_id = "rp-error-capacity";
    let ringing = rp_ring(&mut service, &mut wire, &rp_invite(call_id, false)).await;
    let cseq_before = service
        .call
        .as_ref()
        .unwrap()
        .dialog
        .as_ref()
        .unwrap()
        .remote_cseq();
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().len();
    let mut originals = vec![];
    controls.hold_prack_final.store(true, Ordering::SeqCst);
    for index in 0..15 {
        let mut request = rp_prack(call_id, &ringing, 100 + index);
        request
            .data
            .headers
            .set(Header::new("RAck", "invalid").unwrap());
        let confirmed = index % 2 == 0;
        let event = request_event(request.clone());
        let mut handling = Box::pin(service.handle_event(&event));
        let rejected = tokio::select! {
            result = &mut handling => panic!("error final completed before capacity fixture receipt {result:?}"),
            message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
        };
        assert_eq!(rejected.status, 400);
        let receipt = rp_recv!(receipts);
        assert_eq!((receipt.status, receipt.method), (400, Method::PRACK));
        receipt
            .reply
            .send(if confirmed {
                Ok(())
            } else {
                Err(rp_write_failure())
            })
            .unwrap();
        let result = handling.await;
        if confirmed {
            result.unwrap();
        } else {
            assert!(result.unwrap_err().uncertain);
        }
        originals.push((request, rejected, confirmed));
    }
    let call = service.call.as_ref().unwrap();
    assert_eq!(call.prack_replies.len(), 15);
    assert_eq!(call.dialog.as_ref().unwrap().remote_cseq(), cseq_before);
    assert!(!call.reliable.as_ref().unwrap().pracked);
    assert!(!call.uncertain);
    let mut overflow = rp_prack(call_id, &ringing, 115);
    overflow
        .data
        .headers
        .set(Header::new("RAck", "invalid").unwrap());
    let error = service
        .handle_event(&request_event(overflow))
        .await
        .unwrap_err();
    assert_eq!(error.code, "ims_prack_reply_capacity");
    assert!(
        wire.try_recv().is_err(),
        "never emit a final that cannot retain its transaction identity"
    );
    let call = service.call.as_ref().unwrap();
    assert_eq!(call.prack_replies.len(), 15);
    assert_eq!(call.dialog.as_ref().unwrap().remote_cseq(), cseq_before);
    assert!(!call.reliable.as_ref().unwrap().pracked);
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);

    controls.hold_prack_final.store(false, Ordering::SeqCst);
    let mut valid = rp_prack(call_id, &ringing, 16);
    rp_set_sdp(
        &mut valid,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    );
    service.handle_event(&request_event(valid)).await.unwrap();
    let accepted = rp_recv!(wire).response.unwrap();
    assert_eq!(accepted.status, 200);
    assert!(!accepted.data.body.is_empty());
    let call = service.call.as_ref().unwrap();
    assert_eq!(call.prack_replies.len(), 16);
    assert_eq!(call.dialog.as_ref().unwrap().remote_cseq(), 16);
    assert!(call.reliable.as_ref().unwrap().pracked);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared + 1);
    assert_eq!(media.configured.lock().unwrap().len(), configured.len() + 1);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);

    // All fifteen old outcomes survive the reserved-slot admission. Change
    // each duplicate's RAck/body to prove it cannot become another SDP offer.
    for (mut request, rejected, confirmed) in originals {
        request.data.headers.set(
            Header::new(
                "RAck",
                format!(
                    "{} 14 INVITE",
                    single(&ringing.data.headers, "RSeq").unwrap()
                ),
            )
            .unwrap(),
        );
        rp_set_sdp(&mut request, p5_evs_offer(96, 49202));
        service.handle_event(&request_event(request)).await.unwrap();
        if confirmed {
            assert_eq!(
                proletarius_sip::Message::Response(rp_recv!(wire).response.unwrap()).encode(),
                proletarius_sip::Message::Response(rejected).encode()
            );
        } else {
            assert!(wire.try_recv().is_err());
        }
    }
    assert_eq!(service.call.as_ref().unwrap().prack_replies.len(), 16);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared + 1);
    assert_eq!(media.configured.lock().unwrap().len(), configured.len() + 1);
    service.answer(g()).await.unwrap();
    let final_response = rp_recv!(wire).response.unwrap();
    assert_eq!(final_response.status, 200);
    assert!(final_response.data.body.is_empty());
    let local_tag = tag(single(&ringing.data.headers, "To").unwrap()).unwrap();
    service
        .handle_event(&request_event(rp_request(
            call_id,
            local_tag,
            14,
            Method::ACK,
        )))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true, false]);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared + 1);
    assert_eq!(media.configured.lock().unwrap().len(), configured.len() + 1);
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}
