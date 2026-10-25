// Completed negative INVITE server transactions are independent of both SDP
// commits and Accepted 2xx ACK handling. Tests use only a controlled SIP writer.
macro_rules! ci_recv {
    ($wire:expr) => {
        tokio::time::timeout(Duration::from_secs(2), $wire.recv())
            .await
            .expect("Completed INVITE wire/receipt timeout")
            .expect("Completed INVITE writer closed")
    };
}
#[derive(Default)]
struct CiWriter {
    hold_negative: AtomicBool,
    hold_ringing: AtomicBool,
    hold_bye: AtomicBool,
}
fn ci_fixture() -> (
    Service,
    Arc<FakeMedia>,
    Arc<CiWriter>,
    mpsc::UnboundedReceiver<Wire>,
    mpsc::UnboundedReceiver<oneshot::Sender<std::io::Result<()>>>,
    tokio::task::JoinHandle<()>,
) {
    let (commands, mut input) = mpsc::channel(32);
    let (sent, wire) = mpsc::unbounded_channel();
    let (receipts, controlled) = mpsc::unbounded_channel();
    let media = Arc::new(FakeMedia::default());
    let writer_media = media.clone();
    let controls = Arc::new(CiWriter::default());
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
                    let hold =
                        request.method == Method::BYE && writer.hold_bye.load(Ordering::SeqCst);
                    sent.send(Wire {
                        request: Some(request),
                        response: None,
                        connection: SipConnectionId::Primary,
                    })
                    .unwrap();
                    if hold {
                        receipts.send(reply).unwrap();
                    } else {
                        let _ = reply.send(Ok(()));
                    }
                }
                Command::RespondDialog {
                    generation,
                    connection,
                    response,
                    reply,
                } => {
                    assert_eq!(generation, g());
                    let hold = cseq(&response.data.headers).unwrap().1 == Method::INVITE
                        && ((response.status >= 300
                            && writer.hold_negative.load(Ordering::SeqCst))
                            || (response.status == 180
                                && writer.hold_ringing.load(Ordering::SeqCst)));
                    writer_media
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
                    if hold {
                        receipts.send(reply).unwrap();
                    } else {
                        let _ = reply.send(Ok(()));
                    }
                }
                _ => panic!("Completed INVITE must not borrow tracked SMS transactions"),
            }
        }
    });
    let service = Service::new(g(), config(), commands, media.clone()).unwrap();
    (service, media, controls, wire, controlled, task)
}
fn ci_branch(request: &mut Request, branch: &str) {
    request.data.headers.set(
        Header::new(
            "Via",
            format!("SIP/2.0/TCP peer.ims.test;branch=z9hG4bKci-{branch};rport"),
        )
        .unwrap(),
    );
}
fn ci_reinvite(call_id: &str, local_tag: &str, seq: u32, branch: &str) -> Request {
    let mut request = p5_offer_request(
        call_id,
        local_tag,
        seq,
        Method::INVITE,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    );
    ci_branch(&mut request, branch);
    request
}
fn ci_ack(request: &Request, response: &Response) -> Request {
    let mut ack = request.clone();
    ack.method = Method::ACK;
    ack.data.body.clear();
    ack.data.headers.set(
        Header::new(
            "CSeq",
            format!("{} ACK", cseq(&request.data.headers).unwrap().0),
        )
        .unwrap(),
    );
    ack.data
        .headers
        .set(Header::new("To", single(&response.data.headers, "To").unwrap()).unwrap());
    ack
}
fn ci_cancel(request: &Request) -> Request {
    let mut cancel = request.clone();
    cancel.method = Method::CANCEL;
    cancel.data.body.clear();
    cancel.data.headers.set(
        Header::new(
            "CSeq",
            format!("{} CANCEL", cseq(&request.data.headers).unwrap().0),
        )
        .unwrap(),
    );
    cancel
}
fn ci_unsupported(request: &mut Request) {
    request.data.body = b"v=0\r\no=- 99 1 IN IP4 192.0.2.20\r\ns=peer\r\nc=IN IP4 192.0.2.20\r\nt=0 0\r\nm=audio 49200 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n".to_vec();
}
fn ci_corrected(request: &Request) -> Request {
    let mut corrected = request.clone();
    corrected.data.body = offer();
    corrected
        .data
        .headers
        .set(Header::new("Content-Type", "application/sdp").unwrap());
    corrected
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@peer.ims.test>").unwrap());
    if corrected.data.headers.get("Session-Expires").is_some() {
        corrected
            .data
            .headers
            .set(Header::new("Session-Expires", "180;refresher=uac").unwrap());
    }
    corrected
}
fn ci_write_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "controlled Completed INVITE writer failure",
    )
}

async fn ci_establish(
    service: &mut Service,
    wire: &mut mpsc::UnboundedReceiver<Wire>,
) -> (String, String) {
    service.dial(g(), "+447700900123").await.unwrap();
    let initial = ci_recv!(wire).request.unwrap();
    service
        .handle_event(&response_event(&initial, 200, offer(), None))
        .await
        .unwrap();
    assert_eq!(ci_recv!(wire).request.unwrap().method, Method::ACK);
    assert_eq!(service.snapshot.state, "active");
    (
        single(&initial.data.headers, "Call-ID").unwrap().to_owned(),
        tag(single(&initial.data.headers, "From").unwrap())
            .unwrap()
            .to_owned(),
    )
}

#[tokio::test]
async fn completed_491_is_frozen_after_older_accepted_reinvite_ack() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
    let first = ci_reinvite(&call_id, &local_tag, 2, "accepted");
    service
        .handle_event(&request_event(first.clone()))
        .await
        .unwrap();
    let accepted = ci_recv!(wire).response.unwrap();
    assert_eq!(accepted.status, 200);
    let collision = ci_reinvite(&call_id, &local_tag, 3, "pending");
    service
        .handle_event(&request_event(collision.clone()))
        .await
        .unwrap();
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 491);
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().len();
    let mut ack = ci_ack(&first, &accepted);
    ci_branch(&mut ack, "independent-2xx-ack");
    service.handle_event(&request_event(ack)).await.unwrap();
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
    service
        .handle_event(&request_event(ci_corrected(&collision)))
        .await
        .unwrap();
    assert_eq!(
        proletarius_sip::Message::Response(ci_recv!(wire).response.unwrap()).encode(),
        proletarius_sip::Message::Response(rejected).encode()
    );
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 2);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_branch_b_negative_ack_cannot_confirm_branch_a_accepted200_with_same_cseq() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
    let first = ci_reinvite(&call_id, &local_tag, 2, "a");
    service
        .handle_event(&request_event(first.clone()))
        .await
        .unwrap();
    let accepted = ci_recv!(wire).response.unwrap();
    assert_eq!(accepted.status, 200);
    let collision = ci_reinvite(&call_id, &local_tag, 2, "b");
    service
        .handle_event(&request_event(collision.clone()))
        .await
        .unwrap();
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 491);
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().len();
    service
        .handle_event(&request_event(ci_ack(&collision, &rejected)))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    let call = service.call.as_ref().unwrap();
    assert!(
        !call
            .accepted_reinvites
            .iter()
            .find(|record| record.cseq == 2)
            .unwrap()
            .acked
    );
    assert!(call.pending_reinvite.is_some());
    assert_eq!(call.local_sdp_version, 1);
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    let mut ack = ci_ack(&first, &accepted);
    ci_branch(&mut ack, "a-new-2xx-ack-branch");
    service.handle_event(&request_event(ack)).await.unwrap();
    let call = service.call.as_ref().unwrap();
    assert!(
        call.accepted_reinvites
            .iter()
            .find(|record| record.cseq == 2)
            .unwrap()
            .acked
    );
    assert!(call.pending_reinvite.is_none());
    assert_eq!(call.local_sdp_version, 2);
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_interleaved_400_415_422_488_500_finals_freeze_each_branch_and_cancel_is_a_noop()
{
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().len();
    let mut originals = vec![];
    for (sequence, expected) in [(2, 400), (3, 415), (4, 422), (5, 488), (5, 500)] {
        let mut request = ci_reinvite(
            &call_id,
            &local_tag,
            sequence,
            &format!("status-{expected}"),
        );
        match expected {
            400 => {
                request
                    .data
                    .headers
                    .set(Header::new("Contact", "*").unwrap());
            }
            415 => {
                request
                    .data
                    .headers
                    .set(Header::new("Content-Type", "text/plain").unwrap());
            }
            422 => {
                push(
                    &mut request.data.headers,
                    "Session-Expires",
                    "1;refresher=uac",
                )
                .unwrap();
            }
            488 => ci_unsupported(&mut request),
            _ => {}
        }
        service
            .handle_event(&request_event(request.clone()))
            .await
            .unwrap();
        let rejected = ci_recv!(wire).response.unwrap();
        assert_eq!(rejected.status, expected);
        originals.push((request, rejected));
    }
    let last_cseq = service
        .call
        .as_ref()
        .unwrap()
        .dialog
        .as_ref()
        .unwrap()
        .remote_cseq();
    assert_eq!(last_cseq, 5);
    for (request, rejected) in originals.iter().rev() {
        service
            .handle_event(&request_event(ci_corrected(request)))
            .await
            .unwrap();
        assert_eq!(ci_recv!(wire).response.unwrap(), *rejected);
        service
            .handle_event(&request_event(ci_cancel(request)))
            .await
            .unwrap();
        let cancel = ci_recv!(wire).response.unwrap();
        assert_eq!(
            (cancel.status, cseq(&cancel.data.headers).unwrap().1),
            (200, Method::CANCEL)
        );
        assert!(
            wire.try_recv().is_err(),
            "matched CANCEL cannot replace a negative INVITE final"
        );
        service
            .handle_event(&request_event(request.clone()))
            .await
            .unwrap();
        assert_eq!(ci_recv!(wire).response.unwrap(), *rejected);
    }
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .dialog
            .as_ref()
            .unwrap()
            .remote_cseq(),
        last_cseq
    );
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    assert_eq!(service.snapshot.state, "active");
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_initial_400_488_and_422_without_call_freeze_final_and_cancel_cannot_admit_corrected_request()
 {
    for expected in [400, 488, 422] {
        let (mut service, media, mut wire, task) = fixture();
        let call_id = format!("ci-no-call-{expected}");
        let mut initial = inbound(&call_id, 14, Method::INVITE, None);
        ci_branch(&mut initial, &format!("initial-{expected}"));
        if expected == 400 {
            initial
                .data
                .headers
                .set(Header::new("Contact", "*").unwrap());
        } else if expected == 488 {
            ci_unsupported(&mut initial);
        } else {
            push(
                &mut initial.data.headers,
                "Session-Expires",
                "1;refresher=uac",
            )
            .unwrap();
        }
        service
            .handle_event(&request_event(initial.clone()))
            .await
            .unwrap();
        assert_eq!(ci_recv!(wire).response.unwrap().status, 100);
        let rejected = ci_recv!(wire).response.unwrap();
        assert_eq!(rejected.status, expected);
        assert!(service.call.is_none());
        assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
        for request in [initial.clone(), ci_corrected(&initial)] {
            service.handle_event(&request_event(request)).await.unwrap();
            assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
            assert!(service.call.is_none());
        }
        service
            .handle_event(&request_event(ci_cancel(&initial)))
            .await
            .unwrap();
        let cancel = ci_recv!(wire).response.unwrap();
        assert_eq!(
            (cancel.status, cseq(&cancel.data.headers).unwrap().1),
            (200, Method::CANCEL)
        );
        assert!(wire.try_recv().is_err());
        service
            .handle_event(&request_event(ci_corrected(&initial)))
            .await
            .unwrap();
        assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
        assert!(service.call.is_none());
        assert!(media.prepared.lock().unwrap().is_empty());
        assert!(media.configured.lock().unwrap().is_empty());
        assert!(media.start_modes.lock().unwrap().is_empty());
        assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn completed_busy486_without_replacement_call_remains_frozen_after_original_active_call_ends()
{
    let (mut service, media, mut wire, task) = fixture();
    let (active_id, _) = ci_establish(&mut service, &mut wire).await;
    let mut busy = inbound("ci-busy-other-dialog", 14, Method::INVITE, None);
    ci_branch(&mut busy, "busy");
    service
        .handle_event(&request_event(busy.clone()))
        .await
        .unwrap();
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 486);
    assert_eq!(service.snapshot.call_id, active_id);
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    service.hangup(g()).await.unwrap();
    assert_eq!(ci_recv!(wire).request.unwrap().method, Method::BYE);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service
        .handle_event(&request_event(ci_corrected(&busy)))
        .await
        .unwrap();
    assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
    assert_eq!(service.snapshot.call_id, active_id);
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    service
        .handle_event(&request_event(ci_cancel(&busy)))
        .await
        .unwrap();
    assert_eq!(ci_recv!(wire).response.unwrap().status, 200);
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_negative_ack_requires_its_connection_dialog_branch_and_cseq_and_i0_absorbs_duplicates()
 {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
    let mut request = ci_reinvite(&call_id, &local_tag, 2, "negative-ack");
    ci_unsupported(&mut request);
    service
        .handle_event(&request_event(request.clone()))
        .await
        .unwrap();
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 488);
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().len();
    for mismatch in [
        "connection",
        "from",
        "to",
        "branch",
        "sent-by",
        "uri",
        "cseq",
        "cseq-method",
    ] {
        let mut ack = ci_ack(&request, &rejected);
        match mismatch {
            "from" => {
                ack.data
                    .headers
                    .set(Header::new("From", "<sip:b@ims.test>;tag=wrong-dialog").unwrap());
            }
            "to" => {
                ack.data
                    .headers
                    .set(Header::new("To", "<sip:a@ims.test>;tag=wrong-dialog").unwrap());
            }
            "branch" => ci_branch(&mut ack, "different-negative-ack"),
            "sent-by" => {
                ack.data.headers.set(
                    Header::new(
                        "Via",
                        "SIP/2.0/TCP unrelated.ims.test;branch=z9hG4bKci-negative-ack;rport",
                    )
                    .unwrap(),
                );
            }
            "uri" => {
                ack.uri = Uri::parse("sip:unrelated@ims.test").unwrap();
            }
            "cseq" => {
                ack.data.headers.set(Header::new("CSeq", "3 ACK").unwrap());
            }
            "cseq-method" => {
                ack.data
                    .headers
                    .set(Header::new("CSeq", "2 INVITE").unwrap());
            }
            _ => {}
        }
        let mut event = request_event(ack);
        if mismatch == "connection" {
            let RegisteredSipEvent::Message { connection, .. } = &mut event.event else {
                unreachable!()
            };
            *connection = SipConnectionId::Incoming(99);
        }
        service.handle_event(&event).await.unwrap();
        assert!(wire.try_recv().is_err());
        service
            .handle_event(&request_event(request.clone()))
            .await
            .unwrap();
        assert_eq!(
            ci_recv!(wire).response.unwrap(),
            rejected,
            "unrelated ACK cannot terminate the negative INVITE transaction"
        );
    }
    service
        .handle_event(&request_event(ci_ack(&request, &rejected)))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    service
        .handle_event(&request_event(ci_corrected(&request)))
        .await
        .unwrap();
    assert!(
        wire.try_recv().is_err(),
        "reliable transport I=0 retains only an absorbing tombstone"
    );
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_unknown_first_final_absorbs_but_failed_replay_keeps_original_known_final_and_active_media()
 {
    for first_known in [false, true] {
        let (mut service, media, controls, mut wire, mut receipts, task) = ci_fixture();
        let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
        let mut request = ci_reinvite(&call_id, &local_tag, 2, "write-fault");
        ci_unsupported(&mut request);
        let configured = media.configured.lock().unwrap().clone();
        let prepared = media.prepared.lock().unwrap().len();
        controls.hold_negative.store(true, Ordering::SeqCst);
        let event = request_event(request.clone());
        let mut handling = Box::pin(service.handle_event(&event));
        let rejected = tokio::select! {
            result = &mut handling => panic!("negative INVITE completed before receipt {result:?}"),
            message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
        };
        assert_eq!(rejected.status, 488);
        ci_recv!(receipts)
            .send(if first_known {
                Ok(())
            } else {
                Err(ci_write_error())
            })
            .unwrap();
        let result = handling.await;
        if first_known {
            result.unwrap();
        } else {
            assert!(result.unwrap_err().uncertain);
        }
        if first_known {
            let event = request_event(ci_corrected(&request));
            let mut replaying = Box::pin(service.handle_event(&event));
            let replay = tokio::select! {
                result = &mut replaying => panic!("replay completed before receipt {result:?}"),
                message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
            };
            assert_eq!(replay, rejected);
            ci_recv!(receipts).send(Err(ci_write_error())).unwrap();
            assert!(replaying.await.unwrap_err().uncertain);
        }
        controls.hold_negative.store(false, Ordering::SeqCst);
        service
            .handle_event(&request_event(ci_corrected(&request)))
            .await
            .unwrap();
        if first_known {
            assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
        } else {
            assert!(
                wire.try_recv().is_err(),
                "an unknown first final cannot turn into a newly accepted offer"
            );
        }
        assert_eq!(service.snapshot.state, "active");
        assert!(!service.snapshot.uncertain);
        assert!(!service.call.as_ref().unwrap().uncertain);
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(media.prepared.lock().unwrap().len(), prepared);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
        assert!(wire.try_recv().is_err());
        service.shutdown(g()).await.unwrap();
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn completed_known_receipt_and_unknown_write_start_anchor_h32_without_unsolicited_retries() {
    for confirmed in [false, true] {
        let (mut service, media, controls, mut wire, mut receipts, task) = ci_fixture();
        let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
        let mut request = ci_reinvite(&call_id, &local_tag, 2, "h-timer");
        ci_unsupported(&mut request);
        controls.hold_negative.store(true, Ordering::SeqCst);
        let event = request_event(request.clone());
        let before_write = Instant::now();
        let mut handling = Box::pin(service.handle_event(&event));
        let response = tokio::select! {
            result = &mut handling => panic!("negative final finished before receipt {result:?}"),
            message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
        };
        assert_eq!(response.status, 488);
        let observed = Instant::now();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let released = Instant::now();
        ci_recv!(receipts)
            .send(if confirmed {
                Ok(())
            } else {
                Err(ci_write_error())
            })
            .unwrap();
        let result = handling.await;
        if confirmed {
            result.unwrap();
        } else {
            assert!(result.unwrap_err().uncertain);
        }
        let complete = Instant::now();
        let record = service.completed_invites.back().unwrap();
        assert!(matches!(
            record.phase,
            completed_invite::CompletedPhase::Completed
        ));
        assert_eq!(record.confirmed, confirmed);
        let deadline = record.deadline.unwrap();
        if confirmed {
            assert!(deadline >= released + Duration::from_secs(32));
            assert!(deadline <= complete + Duration::from_secs(32));
        } else {
            assert!(deadline >= before_write + Duration::from_secs(32));
            assert!(deadline <= observed + Duration::from_secs(32));
        }
        controls.hold_negative.store(false, Ordering::SeqCst);
        service
            .tick(g(), deadline - Duration::from_millis(1))
            .await
            .unwrap();
        assert_eq!(service.completed_invites.len(), 1);
        assert!(
            wire.try_recv().is_err(),
            "reliable TCP Completed has no periodic final retransmission"
        );
        service.tick(g(), deadline).await.unwrap();
        assert!(service.completed_invites.is_empty());
        assert_eq!(service.snapshot.state, "active");
        assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
        assert!(wire.try_recv().is_err());
        service.shutdown(g()).await.unwrap();
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn completed_global_capacity_retains_all_64_finals_and_i0_tombstone_until_h_expiry() {
    let (mut service, media, mut wire, task) = fixture();
    let mut first = None;
    let mut last_deadline = Instant::now();
    for index in 0..64 {
        let mut request = inbound(&format!("ci-cap-{index}"), 14, Method::INVITE, None);
        ci_branch(&mut request, &format!("cap-{index}"));
        ci_unsupported(&mut request);
        service
            .handle_event(&request_event(request.clone()))
            .await
            .unwrap();
        assert_eq!(ci_recv!(wire).response.unwrap().status, 100);
        let rejected = ci_recv!(wire).response.unwrap();
        assert_eq!(rejected.status, 488);
        last_deadline = service.completed_invites.back().unwrap().deadline.unwrap();
        if first.is_none() {
            first = Some((request, rejected));
        }
    }
    assert!(service.call.is_none());
    assert_eq!(service.completed_invites.len(), 64);
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    let mut overflow = inbound("ci-cap-overflow", 14, Method::INVITE, None);
    ci_branch(&mut overflow, "cap-overflow");
    ci_unsupported(&mut overflow);
    let error = service
        .handle_event(&request_event(overflow.clone()))
        .await
        .unwrap_err();
    assert_eq!(error.code, "ims_invite_reply_capacity");
    assert!(wire.try_recv().is_err());
    assert_eq!(service.completed_invites.len(), 64);
    let (first_request, first_response) = first.unwrap();
    service
        .handle_event(&request_event(ci_corrected(&first_request)))
        .await
        .unwrap();
    assert_eq!(ci_recv!(wire).response.unwrap(), first_response);
    service
        .handle_event(&request_event(ci_ack(&first_request, &first_response)))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(
        service.completed_invites.len(),
        64,
        "I=0 does not evict the absorbing identity"
    );
    service
        .handle_event(&request_event(ci_corrected(&first_request)))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(
        service
            .handle_event(&request_event(overflow.clone()))
            .await
            .unwrap_err()
            .code,
        "ims_invite_reply_capacity"
    );
    assert!(wire.try_recv().is_err());
    assert!(service.call.is_none());
    service
        .tick(g(), last_deadline + Duration::from_millis(1))
        .await
        .unwrap();
    assert!(service.completed_invites.is_empty());
    service
        .handle_event(&request_event(overflow))
        .await
        .unwrap();
    assert_eq!(ci_recv!(wire).response.unwrap().status, 100);
    assert_eq!(ci_recv!(wire).response.unwrap().status, 488);
    assert_eq!(service.completed_invites.len(), 1);
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    assert!(media.configured.lock().unwrap().is_empty());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_503_at_accepted_capacity_does_not_replace_older400_or_prepare_new_media() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
    let mut bad = ci_reinvite(&call_id, &local_tag, 2, "old400");
    bad.data.headers.set(Header::new("Contact", "*").unwrap());
    service
        .handle_event(&request_event(bad.clone()))
        .await
        .unwrap();
    let first = ci_recv!(wire).response.unwrap();
    assert_eq!(first.status, 400);
    for sequence in 3..=18 {
        let mut request = ci_reinvite(
            &call_id,
            &local_tag,
            sequence,
            &format!("accepted-{sequence}"),
        );
        request.data.body = p5_wb_offer(111, 49200 + sequence as u16, "octet-align=1;mode-set=2,5");
        service
            .handle_event(&request_event(request.clone()))
            .await
            .unwrap();
        let accepted = ci_recv!(wire).response.unwrap();
        assert_eq!(accepted.status, 200);
        service
            .handle_event(&request_event(ci_ack(&request, &accepted)))
            .await
            .unwrap();
    }
    assert_eq!(service.call.as_ref().unwrap().accepted_reinvites.len(), 16);
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().len();
    let overflow = ci_reinvite(&call_id, &local_tag, 19, "accepted-overflow");
    service
        .handle_event(&request_event(overflow.clone()))
        .await
        .unwrap();
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 503);
    assert_eq!(single(&rejected.data.headers, "Retry-After").unwrap(), "1");
    for (request, response) in [
        (ci_corrected(&bad), first),
        (ci_corrected(&overflow), rejected),
    ] {
        service.handle_event(&request_event(request)).await.unwrap();
        assert_eq!(ci_recv!(wire).response.unwrap(), response);
    }
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_terminal_old_dialog_replay_and_negative_ack_cannot_change_new_active_call() {
    let (mut service, media, mut wire, task) = fixture();
    let (old_id, old_tag) = ci_establish(&mut service, &mut wire).await;
    let mut old = ci_reinvite(&old_id, &old_tag, 2, "retired");
    ci_unsupported(&mut old);
    service
        .handle_event(&request_event(old.clone()))
        .await
        .unwrap();
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 488);
    service.hangup(g()).await.unwrap();
    assert_eq!(ci_recv!(wire).request.unwrap().method, Method::BYE);
    let (new_id, _) = ci_establish(&mut service, &mut wire).await;
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().len();
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
    service
        .handle_event(&request_event(ci_corrected(&old)))
        .await
        .unwrap();
    assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
    for mismatch in ["connection", "from", "to", "uri"] {
        let mut changed = ci_corrected(&old);
        match mismatch {
            "from" => {
                changed
                    .data
                    .headers
                    .set(Header::new("From", "<sip:b@ims.test>;tag=other-dialog").unwrap());
            }
            "to" => {
                changed
                    .data
                    .headers
                    .set(Header::new("To", "<sip:a@ims.test>;tag=other-dialog").unwrap());
            }
            "uri" => {
                changed.uri = Uri::parse("sip:other@ims.test").unwrap();
            }
            _ => {}
        }
        let mut event = request_event(changed);
        if mismatch == "connection" {
            let RegisteredSipEvent::Message { connection, .. } = &mut event.event else {
                unreachable!()
            };
            *connection = SipConnectionId::Incoming(99);
        }
        service.handle_event(&event).await.unwrap();
        assert_eq!(
            ci_recv!(wire).response.unwrap().status,
            481,
            "a different connection/dialog/URI cannot replay the old488"
        );
    }
    service
        .handle_event(&request_event(ci_ack(&old, &rejected)))
        .await
        .unwrap();
    service.handle_event(&request_event(old)).await.unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(service.snapshot.call_id, new_id);
    assert_eq!(service.snapshot.state, "active");
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
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_initial_known_or_unknown_rejection_and_ack_never_redrain_failed_media_stop() {
    for confirmed in [false, true] {
        let (mut service, media, controls, mut wire, mut receipts, task) = ci_fixture();
        let mut invite = inbound(
            &format!("ci-stop-fence-{confirmed}"),
            14,
            Method::INVITE,
            None,
        );
        ci_branch(&mut invite, "stop-fence");
        service
            .handle_event(&request_event(invite.clone()))
            .await
            .unwrap();
        assert_eq!(ci_recv!(wire).response.unwrap().status, 100);
        assert_eq!(ci_recv!(wire).response.unwrap().status, 180);
        media.prepare_fail.store(true, Ordering::SeqCst);
        media.stop_fail.store(true, Ordering::SeqCst);
        controls.hold_negative.store(true, Ordering::SeqCst);
        let mut answering = Box::pin(service.answer(g()));
        let rejected = tokio::select! {
            result = &mut answering => panic!("negative answer completed before receipt; error={:?}", result.err()),
            message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
        };
        assert_eq!(rejected.status, 488);
        ci_recv!(receipts)
            .send(if confirmed {
                Ok(())
            } else {
                Err(ci_write_error())
            })
            .unwrap();
        assert!(answering.await.err().unwrap().uncertain);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
        assert!(service.call.as_ref().unwrap().media_allocated);
        assert!(service.call.as_ref().unwrap().uncertain);
        assert_eq!(service.snapshot.state, "failed");
        assert!(service.active());
        assert!(service.dial(g(), "+447700900124").await.is_err());
        controls.hold_negative.store(false, Ordering::SeqCst);
        service
            .handle_event(&request_event(invite.clone()))
            .await
            .unwrap();
        if confirmed {
            assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
        } else {
            assert!(wire.try_recv().is_err());
        }
        service
            .handle_event(&request_event(ci_ack(&invite, &rejected)))
            .await
            .unwrap();
        service
            .handle_event(&request_event(ci_corrected(&invite)))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
        assert!(media.configured.lock().unwrap().is_empty());
        assert!(media.start_modes.lock().unwrap().is_empty());
        media.stop_fail.store(false, Ordering::SeqCst);
        service.shutdown(g()).await.unwrap();
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn completed_i0_tombstone_outlives_h_only_while_same_cseq_accepted_ack_is_still_protected() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
    let now = Instant::now();
    let call = service.call.as_mut().unwrap();
    call.peer_allows_update = false;
    call.session_timer = Some(SessionTimer {
        interval: 180,
        min_se: 90,
        local_refresher: true,
        expires_at: now + Duration::from_secs(180),
        termination_at: now + Duration::from_secs(180),
        refresh_at: Some(now),
        pending: None,
        retry_count: 0,
    });
    service.tick(g(), now).await.unwrap();
    let refresh = ci_recv!(wire).request.unwrap();
    assert_eq!(refresh.method, Method::INVITE);
    let collision = ci_reinvite(&call_id, &local_tag, 2, "earlier-b491");
    service
        .handle_event(&request_event(collision.clone()))
        .await
        .unwrap();
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 491);
    let h_deadline = service.completed_invites.back().unwrap().deadline.unwrap();
    service
        .handle_event(&response_event(&refresh, 200, offer(), None))
        .await
        .unwrap();
    assert_eq!(ci_recv!(wire).request.unwrap().method, Method::ACK);
    let accepted_request = ci_reinvite(&call_id, &local_tag, 2, "later-a200");
    service
        .handle_event(&request_event(accepted_request.clone()))
        .await
        .unwrap();
    let accepted = ci_recv!(wire).response.unwrap();
    assert_eq!(accepted.status, 200);
    let accepted_deadline = service
        .call
        .as_ref()
        .unwrap()
        .accepted_reinvites
        .back()
        .unwrap()
        .deadline;
    assert!(h_deadline < accepted_deadline);
    service
        .handle_event(&request_event(ci_ack(&collision, &rejected)))
        .await
        .unwrap();
    assert!(matches!(
        service.completed_invites.back().unwrap().phase,
        completed_invite::CompletedPhase::Tombstone
    ));
    assert!(service.completed_invites.back().unwrap().response.is_none());
    service.tick(g(), h_deadline).await.unwrap();
    assert_eq!(
        ci_recv!(wire).response.unwrap(),
        accepted,
        "only the Accepted TU may retransmit its200"
    );
    assert_eq!(service.completed_invites.len(), 1);
    assert!(matches!(
        service.completed_invites.back().unwrap().phase,
        completed_invite::CompletedPhase::Tombstone
    ));
    service
        .handle_event(&request_event(ci_ack(&collision, &rejected)))
        .await
        .unwrap();
    assert!(
        !service
            .call
            .as_ref()
            .unwrap()
            .accepted_reinvites
            .back()
            .unwrap()
            .acked
    );
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_some());
    let mut ack = ci_ack(&accepted_request, &accepted);
    ci_branch(&mut ack, "free-2xx-ack");
    service.handle_event(&request_event(ack)).await.unwrap();
    assert!(
        service
            .call
            .as_ref()
            .unwrap()
            .accepted_reinvites
            .back()
            .unwrap()
            .acked
    );
    service
        .tick(g(), accepted_deadline + Duration::from_millis(1))
        .await
        .unwrap();
    assert!(service.completed_invites.is_empty());
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_plain_ringing_write_failure_finalizes500_without_allocating_or_leaking_proceeding()
 {
    for final_known in [false, true] {
        let (mut service, media, controls, mut wire, mut receipts, task) = ci_fixture();
        controls.hold_ringing.store(true, Ordering::SeqCst);
        controls.hold_negative.store(true, Ordering::SeqCst);
        let mut invite = inbound(
            &format!("ci-ringing-failure-{final_known}"),
            21,
            Method::INVITE,
            None,
        );
        ci_branch(&mut invite, "ringing-failure");
        let event = request_event(invite.clone());
        let mut handling = Box::pin(service.handle_event(&event));
        let trying = tokio::select! {
            result = &mut handling => panic!("ringing completed before receipt {result:?}"),
            message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
        };
        assert_eq!(trying.status, 100);
        let ringing = tokio::select! {
            result = &mut handling => panic!("ringing completed before receipt {result:?}"),
            message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
        };
        assert_eq!(ringing.status, 180);
        ci_recv!(receipts).send(Err(ci_write_error())).unwrap();
        let final_response = tokio::select! {
            result = &mut handling => panic!("failed 180 left without final INVITE response {result:?}"),
            message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
        };
        assert_eq!(final_response.status, 500);
        ci_recv!(receipts)
            .send(if final_known {
                Ok(())
            } else {
                Err(ci_write_error())
            })
            .unwrap();
        assert!(handling.await.unwrap_err().uncertain);
        assert_eq!(service.snapshot.state, "failed");
        assert_eq!(service.completed_invites.len(), 1);
        let completed = service.completed_invites.back().unwrap();
        assert_eq!(completed.phase, completed_invite::CompletedPhase::Completed);
        assert_eq!(completed.confirmed, final_known);
        assert!(completed.deadline.is_some());
        assert_eq!(completed.response.as_ref().unwrap(), &final_response);
        // Plain ringing has no allocation to drain. The transaction now has
        // a bounded final lifetime instead of an immortal Proceeding record.
        assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        assert!(service.call.as_ref().unwrap().deadline.is_none());
        assert!(terminal(&service.snapshot.state));
        controls.hold_negative.store(false, Ordering::SeqCst);
        service
            .handle_event(&request_event(ci_corrected(&invite)))
            .await
            .unwrap();
        if final_known {
            assert_eq!(ci_recv!(wire).response.unwrap(), final_response);
        } else {
            assert!(wire.try_recv().is_err());
        }
        service
            .handle_event(&request_event(ci_ack(&invite, &final_response)))
            .await
            .unwrap();
        service.handle_event(&request_event(invite)).await.unwrap();
        assert!(wire.try_recv().is_err());
        assert_eq!(
            service.completed_invites.back().unwrap().phase,
            completed_invite::CompletedPhase::Tombstone
        );
        assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
        assert!(media.prepared.lock().unwrap().is_empty());
        assert!(media.configured.lock().unwrap().is_empty());
        assert!(media.start_modes.lock().unwrap().is_empty());
        service.shutdown(g()).await.unwrap();
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn completed_prepare_and_discard_failure_finalizes488_attempts_bye_and_drains_despite_writer_faults()
 {
    for final_known in [false, true] {
        for bye_known in [false, true] {
            for stop_fails in [false, true] {
                let (mut service, media, controls, mut wire, mut receipts, task) = ci_fixture();
                let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
                let request = ci_reinvite(&call_id, &local_tag, 2, "prepare-discard-failure");
                let configured = media.configured.lock().unwrap().clone();
                let starts = media.start_modes.lock().unwrap().clone();
                let prepared = media.prepared.lock().unwrap().len();
                let prepare_attempts = media
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|event| *event == "prepare")
                    .count();
                let discards = media.discards.load(Ordering::SeqCst);
                let version = service.call.as_ref().unwrap().local_sdp_version;
                let target = service
                    .call
                    .as_ref()
                    .unwrap()
                    .dialog
                    .as_ref()
                    .unwrap()
                    .remote_target
                    .clone();
                media.prepare_fail.store(true, Ordering::SeqCst);
                media.discard_fail.store(true, Ordering::SeqCst);
                media.stop_fail.store(stop_fails, Ordering::SeqCst);
                controls.hold_negative.store(true, Ordering::SeqCst);
                controls.hold_bye.store(true, Ordering::SeqCst);
                let event = request_event(request.clone());
                let mut handling = Box::pin(service.handle_event(&event));
                let rejected = tokio::select! {
                    result = &mut handling => panic!("discard failure skipped final INVITE response {result:?}"),
                    message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().response.unwrap(),
                };
                assert_eq!(rejected.status, 488);
                ci_recv!(receipts)
                    .send(if final_known {
                        Ok(())
                    } else {
                        Err(ci_write_error())
                    })
                    .unwrap();
                let bye = tokio::select! {
                    result = &mut handling => panic!("discard failure skipped BYE attempt {result:?}"),
                    message = tokio::time::timeout(Duration::from_secs(2), wire.recv()) => message.unwrap().unwrap().request.unwrap(),
                };
                assert_eq!(bye.method, Method::BYE);
                assert_eq!(bye.uri, target);
                assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
                ci_recv!(receipts)
                    .send(if bye_known {
                        Ok(())
                    } else {
                        Err(ci_write_error())
                    })
                    .unwrap();
                assert!(handling.await.is_err());
                assert_eq!(service.snapshot.state, "failed");
                assert!(!service.snapshot.media_ready);
                assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
                assert_eq!(
                    media.stopped.load(Ordering::SeqCst),
                    usize::from(!stop_fails)
                );
                let call = service.call.as_ref().unwrap();
                assert_eq!(call.media_allocated, stop_fails);
                assert!(call.pending_reinvite.is_none());
                assert!(call.accepted_reinvites.is_empty());
                assert_eq!(call.local_sdp_version, version);
                if stop_fails {
                    assert!(call.uncertain);
                    assert!(service.active(), "unconfirmed stop retains resource fence");
                }
                let completed = service.completed_invites.back().unwrap();
                assert_eq!(completed.phase, completed_invite::CompletedPhase::Completed);
                assert_eq!(completed.confirmed, final_known);
                assert_eq!(completed.response.as_ref().unwrap(), &rejected);
                assert!(completed.deadline.is_some());
                controls.hold_negative.store(false, Ordering::SeqCst);
                service
                    .handle_event(&request_event(ci_corrected(&request)))
                    .await
                    .unwrap();
                if final_known {
                    assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
                } else {
                    assert!(wire.try_recv().is_err());
                }
                service
                    .handle_event(&request_event(ci_ack(&request, &rejected)))
                    .await
                    .unwrap();
                service.handle_event(&request_event(request)).await.unwrap();
                assert!(
                    wire.try_recv().is_err(),
                    "negative ACK cannot send a second BYE"
                );
                assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
                assert_eq!(media.discards.load(Ordering::SeqCst), discards + 1);
                assert_eq!(media.prepared.lock().unwrap().len(), prepared);
                assert_eq!(
                    media
                        .events
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|event| *event == "prepare")
                        .count(),
                    prepare_attempts + 1
                );
                assert_eq!(*media.configured.lock().unwrap(), configured);
                assert_eq!(*media.start_modes.lock().unwrap(), starts);
                media.stop_fail.store(false, Ordering::SeqCst);
                service.shutdown(g()).await.unwrap();
                assert_eq!(
                    media.stop_attempts.load(Ordering::SeqCst),
                    if stop_fails { 2 } else { 1 }
                );
                assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
                drop(service);
                task.await.unwrap();
            }
        }
    }
}

#[tokio::test]
async fn completed_sdp_version_overflow_freezes500_before_prepare_and_preserves_active_media() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
    service.call.as_mut().unwrap().local_sdp_version = u64::MAX;
    let request = ci_reinvite(&call_id, &local_tag, 2, "version-overflow");
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().clone();
    let events = media
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| !event.starts_with("response"))
        .cloned()
        .collect::<Vec<_>>();
    let starts = media.start_modes.lock().unwrap().clone();
    let discards = media.discards.load(Ordering::SeqCst);
    let token = service.snapshot.audio_token.clone();
    service
        .handle_event(&request_event(request.clone()))
        .await
        .unwrap();
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 500);
    assert_eq!(service.completed_invites.len(), 1);
    let completed = service.completed_invites.back().unwrap();
    assert_eq!(completed.phase, completed_invite::CompletedPhase::Completed);
    assert!(completed.confirmed);
    assert_eq!(completed.response.as_ref().unwrap(), &rejected);
    assert!(completed.deadline.is_some());
    service
        .handle_event(&request_event(ci_corrected(&request)))
        .await
        .unwrap();
    assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
    service
        .handle_event(&request_event(ci_ack(&request, &rejected)))
        .await
        .unwrap();
    service.handle_event(&request_event(request)).await.unwrap();
    assert!(wire.try_recv().is_err());
    let call = service.call.as_ref().unwrap();
    assert_eq!(call.local_sdp_version, u64::MAX);
    assert!(call.pending_reinvite.is_none());
    assert!(call.accepted_reinvites.is_empty());
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.snapshot.audio_token, token);
    assert!(!service.snapshot.uncertain);
    assert_eq!(*media.prepared.lock().unwrap(), prepared);
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(
        media
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| !event.starts_with("response"))
            .cloned()
            .collect::<Vec<_>>(),
        events
    );
    assert_eq!(*media.start_modes.lock().unwrap(), starts);
    assert_eq!(media.discards.load(Ordering::SeqCst), discards);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_only_top_via_identifies_frozen_final_and_negative_ack_for_combined_or_multiple_headers()
 {
    for combined in [false, true] {
        let (mut service, media, mut wire, task) = fixture();
        let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
        let set_vias = |request: &mut Request, lower_branch: &str| {
            let mut headers = Headers::default();
            for header in request
                .data
                .headers
                .iter()
                .filter(|header| !header.is_named("Via"))
            {
                headers.push(header.clone());
            }
            let top = "SIP/2.0/TCP peer.ims.test;branch=z9hG4bKci-top-via;rport";
            let lower = format!("SIP/2.0/TCP lower.ims.test;branch=z9hG4bKci-{lower_branch}");
            if combined {
                headers.prepend(Header::new("Via", format!("{top}, {lower}")).unwrap());
            } else {
                headers.prepend(Header::new("Via", top).unwrap());
                headers.push(Header::new("Via", lower).unwrap());
            }
            request.data.headers = headers;
        };
        let mut request = ci_reinvite(&call_id, &local_tag, 2, "unused");
        ci_unsupported(&mut request);
        set_vias(&mut request, "lower-original");
        let configured = media.configured.lock().unwrap().clone();
        let prepared = media.prepared.lock().unwrap().clone();
        let starts = media.start_modes.lock().unwrap().clone();
        service
            .handle_event(&request_event(request.clone()))
            .await
            .unwrap();
        let rejected = ci_recv!(wire).response.unwrap();
        assert_eq!(rejected.status, 488);
        let deadline = service.completed_invites.back().unwrap().deadline;
        let mut corrected = ci_corrected(&request);
        set_vias(&mut corrected, "lower-corrected");
        service
            .handle_event(&request_event(corrected.clone()))
            .await
            .unwrap();
        assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
        assert_eq!(service.completed_invites.back().unwrap().deadline, deadline);
        let mut ack = ci_ack(&request, &rejected);
        set_vias(&mut ack, "lower-ack");
        service
            .handle_event(&request_event(ack.clone()))
            .await
            .unwrap();
        assert_eq!(
            service.completed_invites.back().unwrap().phase,
            completed_invite::CompletedPhase::Tombstone
        );
        assert!(service.completed_invites.back().unwrap().response.is_none());
        service.handle_event(&request_event(ack)).await.unwrap();
        service
            .handle_event(&request_event(corrected))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
        assert!(service.call.as_ref().unwrap().accepted_reinvites.is_empty());
        assert_eq!(service.snapshot.state, "active");
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(*media.prepared.lock().unwrap(), prepared);
        assert_eq!(*media.start_modes.lock().unwrap(), starts);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
        service.shutdown(g()).await.unwrap();
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn completed_expired_h_with_live_cancel_memo_schedules_only_future_memo_deadline() {
    let (mut service, media, mut wire, task) = fixture();
    let mut request = inbound("ci-cancel-memo-deadline", 14, Method::INVITE, None);
    ci_branch(&mut request, "cancel-memo-deadline");
    ci_unsupported(&mut request);
    service
        .handle_event(&request_event(request.clone()))
        .await
        .unwrap();
    assert_eq!(ci_recv!(wire).response.unwrap().status, 100);
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 488);
    assert!(service.call.is_none());
    let before_cancel = Instant::now();
    let cancel = ci_cancel(&request);
    service
        .handle_event(&request_event(cancel.clone()))
        .await
        .unwrap();
    let after_cancel = Instant::now();
    let canceled = ci_recv!(wire).response.unwrap();
    assert_eq!(canceled.status, 200);
    assert_eq!(cseq(&canceled.data.headers).unwrap().1, Method::CANCEL);
    // Advance the INVITE H independently of the later CANCEL receipt. No real
    // wait or access to the private memo is needed to exercise its scheduler.
    let now = Instant::now();
    let expired_h = now - Duration::from_millis(1);
    service.completed_invites.back_mut().unwrap().deadline = Some(expired_h);
    service.tick(g(), now).await.unwrap();
    let record = service.completed_invites.back().unwrap();
    assert_eq!(record.phase, completed_invite::CompletedPhase::Tombstone);
    assert!(record.response.is_none());
    assert_eq!(record.deadline, Some(expired_h));
    let memo_deadline = service.completed_next_deadline().unwrap();
    assert!(memo_deadline >= before_cancel + Duration::from_secs(32));
    assert!(memo_deadline <= after_cancel + Duration::from_secs(32));
    assert_eq!(service.next_deadline(), Some(memo_deadline));
    assert!(
        memo_deadline > Instant::now(),
        "expired H must not cause a busy tick"
    );
    service.handle_event(&request_event(cancel)).await.unwrap();
    assert_eq!(ci_recv!(wire).response.unwrap(), canceled);
    assert_eq!(service.completed_next_deadline(), Some(memo_deadline));
    service.handle_event(&request_event(request)).await.unwrap();
    assert!(wire.try_recv().is_err());
    service
        .tick(g(), memo_deadline + Duration::from_millis(1))
        .await
        .unwrap();
    assert!(service.completed_invites.is_empty());
    assert!(service.completed_next_deadline().is_none());
    assert!(service.next_deadline().is_none());
    assert!(wire.try_recv().is_err());
    assert!(service.call.is_none());
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
    assert!(media.prepared.lock().unwrap().is_empty());
    assert!(media.configured.lock().unwrap().is_empty());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_cancel_requires_uri_original_to_tag_connection_and_branch_in_proceeding_and_completed()
 {
    let (mut service, media, mut wire, task) = fixture();
    let mut invite = inbound("ci-cancel-identity", 14, Method::INVITE, None);
    ci_branch(&mut invite, "cancel-identity");
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    assert_eq!(ci_recv!(wire).response.unwrap().status, 100);
    assert_eq!(ci_recv!(wire).response.unwrap().status, 180);
    let original_deadline = service.call.as_ref().unwrap().deadline;
    let mut unrelated = vec![];
    for mismatch in ["uri", "to-tag", "connection", "branch"] {
        let mut cancel = ci_cancel(&invite);
        match mismatch {
            "uri" => cancel.uri = Uri::parse("sip:unrelated@ims.test").unwrap(),
            "to-tag" => cancel
                .data
                .headers
                .set(Header::new("To", "<sip:a@ims.test>;tag=unrelated").unwrap()),
            "branch" => ci_branch(&mut cancel, "unrelated-cancel"),
            _ => {}
        }
        let mut event = request_event(cancel);
        if mismatch == "connection" {
            let RegisteredSipEvent::Message { connection, .. } = &mut event.event else {
                unreachable!()
            };
            *connection = SipConnectionId::Incoming(99);
        }
        service.handle_event(&event).await.unwrap();
        let response = ci_recv!(wire).response.unwrap();
        assert_eq!(
            response.status, 481,
            "{mismatch} cannot cancel the pending INVITE"
        );
        assert_eq!(cseq(&response.data.headers).unwrap().1, Method::CANCEL);
        assert!(wire.try_recv().is_err());
        assert_eq!(service.snapshot.state, "incoming");
        let call = service.call.as_ref().unwrap();
        assert!(
            matches!(&call.tracker, Tracker::Incoming(tracker) if tracker.phase() == IncomingCallPhase::Ringing)
        );
        assert_eq!(call.deadline, original_deadline);
        assert_eq!(service.completed_invites.len(), 1);
        assert_eq!(
            service.completed_invites.back().unwrap().phase,
            completed_invite::CompletedPhase::Proceeding
        );
        unrelated.push(event);
    }
    let valid_cancel = ci_cancel(&invite);
    service
        .handle_event(&request_event(valid_cancel.clone()))
        .await
        .unwrap();
    let canceled = ci_recv!(wire).response.unwrap();
    assert_eq!(canceled.status, 200);
    assert_eq!(cseq(&canceled.data.headers).unwrap().1, Method::CANCEL);
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 487);
    assert_eq!(cseq(&rejected.data.headers).unwrap().1, Method::INVITE);
    assert!(terminal(&service.snapshot.state));
    assert_eq!(
        service.completed_invites.back().unwrap().phase,
        completed_invite::CompletedPhase::Completed
    );
    for event in unrelated {
        service.handle_event(&event).await.unwrap();
        let response = ci_recv!(wire).response.unwrap();
        assert_eq!(
            response.status, 481,
            "unrelated CANCEL cannot borrow cached transaction 200"
        );
        assert_eq!(cseq(&response.data.headers).unwrap().1, Method::CANCEL);
        assert!(wire.try_recv().is_err());
        assert_eq!(service.completed_invites.len(), 1);
        assert_eq!(
            service
                .completed_invites
                .back()
                .unwrap()
                .response
                .as_ref()
                .unwrap(),
            &rejected
        );
    }
    service
        .handle_event(&request_event(valid_cancel))
        .await
        .unwrap();
    assert_eq!(ci_recv!(wire).response.unwrap(), canceled);
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
    assert!(wire.try_recv().is_err());
    assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
    assert!(media.prepared.lock().unwrap().is_empty());
    assert!(media.configured.lock().unwrap().is_empty());
    assert!(media.start_modes.lock().unwrap().is_empty());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

fn ci_assert_same_cseq_accepted_still_waiting(service: &Service) {
    let call = service.call.as_ref().unwrap();
    assert!(
        !call
            .accepted_reinvites
            .iter()
            .find(|record| record.cseq == 2)
            .unwrap()
            .acked
    );
    assert_eq!(call.pending_reinvite.as_ref().unwrap().cseq, 2);
    assert_eq!(call.local_sdp_version, 1);
    assert_eq!(service.snapshot.state, "active");
}

#[tokio::test]
async fn completed_equivalent_via_formats_replay_exact_finals_and_negative_ack_never_confirms_same_cseq200()
 {
    for (name, canonical, equivalent) in [
        (
            "tcp-case",
            "SIP/2.0/TCP peer.ims.test:5090",
            "SIP/2.0/tcp peer.ims.test:5090",
        ),
        (
            "protocol-case",
            "SIP/2.0/TCP peer.ims.test:5090",
            "sip/2.0/TcP peer.ims.test:5090",
        ),
        (
            "slash-space",
            "SIP/2.0/TCP peer.ims.test:5090",
            "SIP / 2.0 / TCP peer.ims.test:5090",
        ),
        (
            "slash-tabs",
            "SIP/2.0/TCP peer.ims.test:5090",
            "SIP\t/\t2.0\t/\tTCP\tpeer.ims.test:5090",
        ),
        (
            "host-case",
            "SIP/2.0/TCP peer.ims.test:5090",
            "SIP/2.0/TCP PeEr.ImS.TeSt:5090",
        ),
        (
            "colon-lws",
            "SIP/2.0/TCP peer.ims.test:5090",
            "SIP/2.0/TCP peer.ims.test \t: \t5090",
        ),
        (
            "numeric-port",
            "SIP/2.0/TCP peer.ims.test:5090",
            "SIP/2.0/TCP peer.ims.test:05090",
        ),
        (
            "omitted-port",
            "SIP/2.0/TCP peer.ims.test",
            "sip / 2.0 / tcp\tPEER.IMS.TEST",
        ),
        (
            "ipv4",
            "SIP/2.0/TCP 192.0.2.20:5090",
            "sip / 2.0 / tcp\t192.0.2.20 \t: 5090",
        ),
        (
            "ipv4-numeric",
            "SIP/2.0/TCP 192.0.2.20:5090",
            "SIP/2.0/TCP 192.000.002.020:5090",
        ),
        (
            "ipv6-expanded",
            "SIP/2.0/TCP [2001:db8::abcd]:5090",
            "SIP/2.0/TCP [2001:0DB8:0000:0000:0000:0000:0000:ABCD]:5090",
        ),
        (
            "ipv6-case",
            "SIP/2.0/TCP [2001:db8::abcd]:5090",
            "SIP/2.0/TCP [2001:DB8::ABCD]:5090",
        ),
        (
            "ipv6-colon-lws",
            "SIP/2.0/TCP [2001:db8::abcd]:5090",
            "sip / 2.0 / tcp\t[2001:DB8::ABCD] \t: \t5090",
        ),
    ] {
        let (mut service, media, mut wire, task) = fixture();
        let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
        let accepted_request = ci_reinvite(&call_id, &local_tag, 2, "equivalent-a");
        service
            .handle_event(&request_event(accepted_request.clone()))
            .await
            .unwrap();
        let accepted = ci_recv!(wire).response.unwrap();
        assert_eq!(accepted.status, 200);
        let mut collision = ci_reinvite(&call_id, &local_tag, 2, "unused");
        let canonical = format!("{canonical};branch=z9hG4bKci-via-B;rport");
        let equivalent = format!("{equivalent};branch=z9hG4bKci-via-B;rport");
        collision
            .data
            .headers
            .set(Header::new("Via", &canonical).unwrap());
        service
            .handle_event(&request_event(collision.clone()))
            .await
            .unwrap();
        let rejected = ci_recv!(wire).response.unwrap();
        assert_eq!(rejected.status, 491, "{name}");
        let original_bytes = proletarius_sip::Message::Response(rejected.clone()).encode();
        let deadline = service.completed_invites.back().unwrap().deadline;
        let configured = media.configured.lock().unwrap().clone();
        let prepared = media.prepared.lock().unwrap().clone();
        let starts = media.start_modes.lock().unwrap().clone();
        let token = service.snapshot.audio_token.clone();
        let mut replay = ci_corrected(&collision);
        replay
            .data
            .headers
            .set(Header::new("Via", &equivalent).unwrap());
        service
            .handle_event(&request_event(replay.clone()))
            .await
            .unwrap();
        let repeated = ci_recv!(wire).response.unwrap();
        assert_eq!(
            proletarius_sip::Message::Response(repeated).encode(),
            original_bytes,
            "{name}: full final response stays frozen"
        );
        assert_eq!(
            service.completed_invites.len(),
            1,
            "{name}: equivalent INVITE does not reserve a second transaction"
        );
        assert_eq!(service.completed_invites.back().unwrap().deadline, deadline);
        ci_assert_same_cseq_accepted_still_waiting(&service);

        let mut cancel = ci_cancel(&collision);
        cancel
            .data
            .headers
            .set(Header::new("Via", &equivalent).unwrap());
        service.handle_event(&request_event(cancel)).await.unwrap();
        let canceled = ci_recv!(wire).response.unwrap();
        assert_eq!(canceled.status, 200, "{name}");
        assert_eq!(cseq(&canceled.data.headers).unwrap().1, Method::CANCEL);
        let cancel_bytes = proletarius_sip::Message::Response(canceled).encode();
        service
            .handle_event(&request_event(ci_cancel(&collision)))
            .await
            .unwrap();
        let repeated_cancel = ci_recv!(wire).response.unwrap();
        assert_eq!(
            proletarius_sip::Message::Response(repeated_cancel).encode(),
            cancel_bytes,
            "{name}: CANCEL memo stays byte-identical"
        );
        ci_assert_same_cseq_accepted_still_waiting(&service);

        let mut ack = ci_ack(&collision, &rejected);
        ack.data
            .headers
            .set(Header::new("Via", &equivalent).unwrap());
        service
            .handle_event(&request_event(ack.clone()))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
        assert_eq!(
            service.completed_invites.back().unwrap().phase,
            completed_invite::CompletedPhase::Tombstone,
            "{name}"
        );
        assert!(service.completed_invites.back().unwrap().response.is_none());
        ci_assert_same_cseq_accepted_still_waiting(&service);
        service.handle_event(&request_event(ack)).await.unwrap();
        service.handle_event(&request_event(replay)).await.unwrap();
        assert!(wire.try_recv().is_err());
        ci_assert_same_cseq_accepted_still_waiting(&service);
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(*media.prepared.lock().unwrap(), prepared);
        assert_eq!(*media.start_modes.lock().unwrap(), starts);
        assert_eq!(service.snapshot.audio_token, token);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
        let mut accepted_ack = ci_ack(&accepted_request, &accepted);
        ci_branch(&mut accepted_ack, "fresh-accepted-ack");
        service
            .handle_event(&request_event(accepted_ack))
            .await
            .unwrap();
        let call = service.call.as_ref().unwrap();
        assert!(call.accepted_reinvites.back().unwrap().acked);
        assert!(call.pending_reinvite.is_none());
        assert_eq!(call.local_sdp_version, 2);
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(*media.prepared.lock().unwrap(), prepared);
        assert_eq!(*media.start_modes.lock().unwrap(), starts);
        service.shutdown(g()).await.unwrap();
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn completed_malformed_via_negative_ack_is_absorbed_before_same_cseq_accepted200() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
    let accepted_request = ci_reinvite(&call_id, &local_tag, 2, "malformed-a");
    service
        .handle_event(&request_event(accepted_request.clone()))
        .await
        .unwrap();
    let accepted = ci_recv!(wire).response.unwrap();
    assert_eq!(accepted.status, 200);
    let collision = ci_reinvite(&call_id, &local_tag, 2, "malformed-b");
    service
        .handle_event(&request_event(collision.clone()))
        .await
        .unwrap();
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 491);
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().clone();
    let starts = media.start_modes.lock().unwrap().clone();
    let deadline = service.completed_invites.back().unwrap().deadline;
    for via in [
        "SIP/2.0/TCP peer.ims.test;rport",
        "SIP/2.0/TCP peer.ims.test;branch=;rport",
        "SIP/2.0/TCP peer.ims.test;branch=z9hG4bKci-malformed-b;branch=z9hG4bKci-malformed-b",
        "SIP/2.0/TCP peer.ims.test;branch=invalid branch",
        "SIP/2.0/TCP peer.ims.test;branch=\"z9hG4bKci-malformed-b\"",
        "SIP/2.0/UDP peer.ims.test;branch=z9hG4bKci-malformed-b",
        "SIP/2.0/TCP peer.ims.test:invalid;branch=z9hG4bKci-malformed-b",
        "SIP/2.0/TCP peer.ims.test:65536;branch=z9hG4bKci-malformed-b",
        "SIP/2.0/TCP [2001:db8::1;branch=z9hG4bKci-malformed-b",
    ] {
        let mut ack = ci_ack(&collision, &rejected);
        ack.data.headers.set(Header::new("Via", via).unwrap());
        service.handle_event(&request_event(ack)).await.unwrap();
        assert!(wire.try_recv().is_err());
        ci_assert_same_cseq_accepted_still_waiting(&service);
        let record = service.completed_invites.back().unwrap();
        assert_eq!(
            record.phase,
            completed_invite::CompletedPhase::Completed,
            "{via}"
        );
        assert_eq!(record.deadline, deadline);
        assert_eq!(record.response.as_ref().unwrap(), &rejected);
        service
            .handle_event(&request_event(collision.clone()))
            .await
            .unwrap();
        assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(*media.prepared.lock().unwrap(), prepared);
        assert_eq!(*media.start_modes.lock().unwrap(), starts);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
    }
    service
        .handle_event(&request_event(ci_ack(&collision, &rejected)))
        .await
        .unwrap();
    ci_assert_same_cseq_accepted_still_waiting(&service);
    let mut accepted_ack = ci_ack(&accepted_request, &accepted);
    ci_branch(&mut accepted_ack, "different-valid-accepted-branch");
    service
        .handle_event(&request_event(accepted_ack))
        .await
        .unwrap();
    assert!(
        service
            .call
            .as_ref()
            .unwrap()
            .accepted_reinvites
            .back()
            .unwrap()
            .acked
    );
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(*media.prepared.lock().unwrap(), prepared);
    assert_eq!(*media.start_modes.lock().unwrap(), starts);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_valid_different_branch_host_or_port_cannot_borrow_negative_invite_or_cancel_cache()
 {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
    let mut request = ci_reinvite(&call_id, &local_tag, 2, "unused");
    ci_unsupported(&mut request);
    request.data.headers.set(
        Header::new(
            "Via",
            "SIP/2.0/TCP peer.ims.test:5060;branch=z9hG4bKci-Sensitive",
        )
        .unwrap(),
    );
    service
        .handle_event(&request_event(request.clone()))
        .await
        .unwrap();
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 488);
    assert!(service.call.as_ref().unwrap().accepted_reinvites.is_empty());
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().clone();
    let starts = media.start_modes.lock().unwrap().clone();
    let deadline = service.completed_invites.front().unwrap().deadline;
    for via in [
        "SIP/2.0/TCP peer.ims.test:5060;branch=z9hG4bKci-sensitive",
        "SIP/2.0/TCP unrelated.ims.test:5060;branch=z9hG4bKci-Sensitive",
        "SIP/2.0/TCP peer.ims.test:5061;branch=z9hG4bKci-Sensitive",
        "SIP/2.0/TCP peer.ims.test;branch=z9hG4bKci-Sensitive",
        "SIP/2.0/TCP [2001:db8::20]:5060;branch=z9hG4bKci-Sensitive",
    ] {
        let mut ack = ci_ack(&request, &rejected);
        ack.data.headers.set(Header::new("Via", via).unwrap());
        service.handle_event(&request_event(ack)).await.unwrap();
        assert!(wire.try_recv().is_err());
        assert_eq!(
            service.completed_invites.front().unwrap().phase,
            completed_invite::CompletedPhase::Completed,
            "{via}"
        );
        assert_eq!(
            service.completed_invites.front().unwrap().deadline,
            deadline
        );
        let mut cancel = ci_cancel(&request);
        cancel.data.headers.set(Header::new("Via", via).unwrap());
        service.handle_event(&request_event(cancel)).await.unwrap();
        let missing = ci_recv!(wire).response.unwrap();
        assert_eq!(
            missing.status, 481,
            "{via}: unrelated CANCEL cannot borrow the negative INVITE"
        );
        assert_eq!(cseq(&missing.data.headers).unwrap().1, Method::CANCEL);
        let mut changed = ci_corrected(&request);
        changed.data.headers.set(Header::new("Via", via).unwrap());
        service.handle_event(&request_event(changed)).await.unwrap();
        let independent = ci_recv!(wire).response.unwrap();
        assert_eq!(
            independent.status, 500,
            "{via}: a different transaction gets its own stale-CSeq final"
        );
        service
            .handle_event(&request_event(request.clone()))
            .await
            .unwrap();
        assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
        assert_eq!(
            service
                .completed_invites
                .front()
                .unwrap()
                .response
                .as_ref()
                .unwrap(),
            &rejected
        );
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(*media.prepared.lock().unwrap(), prepared);
        assert_eq!(*media.start_modes.lock().unwrap(), starts);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
    }
    service
        .handle_event(&request_event(ci_ack(&request, &rejected)))
        .await
        .unwrap();
    assert_eq!(
        service.completed_invites.front().unwrap().phase,
        completed_invite::CompletedPhase::Tombstone
    );
    assert_eq!(service.snapshot.state, "active");
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn completed_proceeding_and_accepted_initial_invite_share_canonical_via_matching_and_reject_bad_cancel()
 {
    for equivalent in [
        "SIP/2.0/tcp peer.ims.test:5090;branch=z9hG4bKci-initial-via;rport",
        "sip \t/ 2.0 /\tTcP\tPeEr.ImS.TeSt \t: \t5090;branch=z9hG4bKci-initial-via;rport",
    ] {
        for finish_by_cancel in [false, true] {
            let (mut service, media, mut wire, task) = fixture();
            let mut invite = inbound("ci-initial-canonical-via", 14, Method::INVITE, None);
            invite.data.headers.set(
                Header::new(
                    "Via",
                    "SIP/2.0/TCP peer.ims.test:5090;branch=z9hG4bKci-initial-via;rport",
                )
                .unwrap(),
            );
            service
                .handle_event(&request_event(invite.clone()))
                .await
                .unwrap();
            assert_eq!(ci_recv!(wire).response.unwrap().status, 100);
            let ringing = ci_recv!(wire).response.unwrap();
            assert_eq!(ringing.status, 180);
            let binding = service.call.as_ref().unwrap().binding.clone();
            let local_tag = service.call.as_ref().unwrap().local_tag.clone();
            let deadline = service.call.as_ref().unwrap().deadline;
            let mut repeated_invite = invite.clone();
            repeated_invite
                .data
                .headers
                .set(Header::new("Via", equivalent).unwrap());
            service
                .handle_event(&request_event(repeated_invite.clone()))
                .await
                .unwrap();
            let repeated_ringing = ci_recv!(wire).response.unwrap();
            assert_eq!(repeated_ringing.status, 180);
            assert_eq!(
                single(&repeated_ringing.data.headers, "To").unwrap(),
                single(&ringing.data.headers, "To").unwrap()
            );
            assert_eq!(service.completed_invites.len(), 1);
            assert_eq!(
                service.completed_invites.front().unwrap().phase,
                completed_invite::CompletedPhase::Proceeding
            );
            assert!(service.call.as_ref().unwrap().binding == binding);
            assert_eq!(service.call.as_ref().unwrap().local_tag, local_tag);
            assert_eq!(service.call.as_ref().unwrap().deadline, deadline);
            assert!(service.retired.is_empty());
            assert_eq!(service.snapshot.state, "incoming");
            assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
            assert!(media.prepared.lock().unwrap().is_empty());
            assert!(media.configured.lock().unwrap().is_empty());
            assert!(media.start_modes.lock().unwrap().is_empty());
            for bad_via in [
                "SIP/2.0/TCP peer.ims.test:5090;branch=z9hG4bKci-initial-via;branch=z9hG4bKci-initial-via",
                "SIP/2.0/UDP peer.ims.test:5090;branch=z9hG4bKci-initial-via;rport",
            ] {
                let mut bad_cancel = ci_cancel(&invite);
                bad_cancel
                    .data
                    .headers
                    .set(Header::new("Via", bad_via).unwrap());
                service
                    .handle_event(&request_event(bad_cancel))
                    .await
                    .unwrap();
                let rejected_cancel = ci_recv!(wire).response.unwrap();
                assert_eq!(rejected_cancel.status, 481, "{bad_via}");
                assert_eq!(
                    cseq(&rejected_cancel.data.headers).unwrap().1,
                    Method::CANCEL
                );
                assert!(wire.try_recv().is_err());
                let call = service.call.as_ref().unwrap();
                assert!(
                    matches!(&call.tracker, Tracker::Incoming(tracker) if tracker.phase() == IncomingCallPhase::Ringing)
                );
                assert_eq!(call.deadline, deadline);
                assert!(call.binding == binding);
                assert_eq!(service.snapshot.state, "incoming");
                assert_eq!(service.completed_invites.len(), 1);
                assert_eq!(
                    service.completed_invites.front().unwrap().phase,
                    completed_invite::CompletedPhase::Proceeding
                );
                assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
                assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
            }
            if finish_by_cancel {
                service
                    .handle_event(&request_event(ci_cancel(&repeated_invite)))
                    .await
                    .unwrap();
                let canceled = ci_recv!(wire).response.unwrap();
                assert_eq!(canceled.status, 200);
                assert_eq!(cseq(&canceled.data.headers).unwrap().1, Method::CANCEL);
                let rejected = ci_recv!(wire).response.unwrap();
                assert_eq!(rejected.status, 487);
                assert_eq!(cseq(&rejected.data.headers).unwrap().1, Method::INVITE);
                assert!(terminal(&service.snapshot.state));
                assert_eq!(service.completed_invites.len(), 1);
                assert_eq!(
                    service.completed_invites.front().unwrap().phase,
                    completed_invite::CompletedPhase::Completed
                );
                let cancel_bytes = proletarius_sip::Message::Response(canceled).encode();
                let final_bytes = proletarius_sip::Message::Response(rejected).encode();
                for duplicate in [invite.clone(), repeated_invite] {
                    service
                        .handle_event(&request_event(ci_cancel(&duplicate)))
                        .await
                        .unwrap();
                    assert_eq!(
                        proletarius_sip::Message::Response(ci_recv!(wire).response.unwrap())
                            .encode(),
                        cancel_bytes
                    );
                    service
                        .handle_event(&request_event(duplicate))
                        .await
                        .unwrap();
                    assert_eq!(
                        proletarius_sip::Message::Response(ci_recv!(wire).response.unwrap())
                            .encode(),
                        final_bytes
                    );
                }
                assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
                assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
            } else {
                service.answer(g()).await.unwrap();
                let accepted = ci_recv!(wire).response.unwrap();
                assert_eq!(accepted.status, 200);
                assert_eq!(service.snapshot.state, "connecting");
                assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
                assert_eq!(media.prepared.lock().unwrap().len(), 1);
                assert_eq!(media.configured.lock().unwrap().len(), 1);
                assert_eq!(*media.start_modes.lock().unwrap(), [true]);
                let final_bytes = proletarius_sip::Message::Response(accepted.clone()).encode();
                let token = service.snapshot.audio_token.clone();
                let pending = service
                    .call
                    .as_ref()
                    .unwrap()
                    .initial_answer
                    .as_ref()
                    .unwrap();
                let accepted_deadline = pending.deadline;
                let retry_at = pending.next_response_retry;
                let retry_interval = pending.response_retry_interval;
                assert!(service.completed_invites.is_empty());
                for duplicate in [invite.clone(), repeated_invite.clone()] {
                    service
                        .handle_event(&request_event(duplicate))
                        .await
                        .unwrap();
                    assert!(
                        wire.try_recv().is_err(),
                        "Accepted INVITE duplicates wait for TU retransmission"
                    );
                    let call = service.call.as_ref().unwrap();
                    let pending = call.initial_answer.as_ref().unwrap();
                    assert_eq!(pending.deadline, accepted_deadline);
                    assert_eq!(pending.next_response_retry, retry_at);
                    assert_eq!(pending.response_retry_interval, retry_interval);
                    assert_eq!(
                        proletarius_sip::Message::Response(call.initial_reply.clone().unwrap())
                            .encode(),
                        final_bytes
                    );
                    assert!(call.binding == binding);
                    assert_eq!(service.snapshot.audio_token, token);
                    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
                    assert_eq!(media.prepared.lock().unwrap().len(), 1);
                    assert_eq!(media.configured.lock().unwrap().len(), 1);
                    assert_eq!(*media.start_modes.lock().unwrap(), [true]);
                }
                service
                    .handle_event(&request_event(ci_ack(&repeated_invite, &accepted)))
                    .await
                    .unwrap();
                assert_eq!(service.snapshot.state, "active");
                assert_eq!(*media.start_modes.lock().unwrap(), [true, false]);
                service
                    .handle_event(&request_event(repeated_invite))
                    .await
                    .unwrap();
                assert!(wire.try_recv().is_err());
                assert_eq!(media.prepared.lock().unwrap().len(), 1);
                assert_eq!(media.configured.lock().unwrap().len(), 1);
                assert_eq!(*media.start_modes.lock().unwrap(), [true, false]);
                assert_eq!(service.snapshot.audio_token, token);
                assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
            }
            assert!(wire.try_recv().is_err());
            service.shutdown(g()).await.unwrap();
            assert_eq!(
                media.stop_attempts.load(Ordering::SeqCst),
                usize::from(!finish_by_cancel)
            );
            drop(service);
            task.await.unwrap();
        }
    }
}

#[tokio::test]
async fn completed_known_negative_branch_absorbs_mismatched_uri_or_tags_without_confirming_same_cseq200()
 {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = ci_establish(&mut service, &mut wire).await;
    let accepted_request = ci_reinvite(&call_id, &local_tag, 2, "uri-guard-a");
    service
        .handle_event(&request_event(accepted_request.clone()))
        .await
        .unwrap();
    let accepted = ci_recv!(wire).response.unwrap();
    assert_eq!(accepted.status, 200);
    let collision = ci_reinvite(&call_id, &local_tag, 2, "uri-guard-b");
    service
        .handle_event(&request_event(collision.clone()))
        .await
        .unwrap();
    let rejected = ci_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 491);
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().clone();
    let starts = media.start_modes.lock().unwrap().clone();
    let token = service.snapshot.audio_token.clone();
    let deadline = service.completed_invites.back().unwrap().deadline;
    for mismatch in [
        "scheme-case",
        "host-case",
        "different-uri",
        "to-tag",
        "from-tag",
    ] {
        let mut ack = ci_ack(&collision, &rejected);
        match mismatch {
            "scheme-case" => ack.uri.scheme.make_ascii_uppercase(),
            "host-case" => ack.uri.host.make_ascii_uppercase(),
            "different-uri" => ack.uri = Uri::parse("sip:unrelated@other.ims.test").unwrap(),
            "to-tag" => ack
                .data
                .headers
                .set(Header::new("To", "<sip:a@ims.test>;tag=unrelated-local-tag").unwrap()),
            "from-tag" => ack
                .data
                .headers
                .set(Header::new("From", "<sip:b@ims.test>;tag=unrelated-remote-tag").unwrap()),
            _ => unreachable!(),
        }
        service.handle_event(&request_event(ack)).await.unwrap();
        assert!(wire.try_recv().is_err());
        ci_assert_same_cseq_accepted_still_waiting(&service);
        let completed = service.completed_invites.back().unwrap();
        assert_eq!(
            completed.phase,
            completed_invite::CompletedPhase::Completed,
            "{mismatch}: only a strict negative ACK may tombstone B"
        );
        assert_eq!(completed.response.as_ref().unwrap(), &rejected);
        assert_eq!(completed.deadline, deadline);
        service
            .handle_event(&request_event(collision.clone()))
            .await
            .unwrap();
        assert_eq!(ci_recv!(wire).response.unwrap(), rejected);
        assert_eq!(service.completed_invites.len(), 1);
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(*media.prepared.lock().unwrap(), prepared);
        assert_eq!(*media.start_modes.lock().unwrap(), starts);
        assert_eq!(service.snapshot.audio_token, token);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
    }
    service
        .handle_event(&request_event(ci_ack(&collision, &rejected)))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    ci_assert_same_cseq_accepted_still_waiting(&service);
    assert_eq!(
        service.completed_invites.back().unwrap().phase,
        completed_invite::CompletedPhase::Tombstone
    );
    assert!(service.completed_invites.back().unwrap().response.is_none());
    let mut accepted_ack = ci_ack(&accepted_request, &accepted);
    ci_branch(&mut accepted_ack, "uri-guard-new-2xx-ack");
    service
        .handle_event(&request_event(accepted_ack))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    let call = service.call.as_ref().unwrap();
    assert!(call.accepted_reinvites.back().unwrap().acked);
    assert!(call.pending_reinvite.is_none());
    assert_eq!(call.local_sdp_version, 2);
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(*media.prepared.lock().unwrap(), prepared);
    assert_eq!(*media.start_modes.lock().unwrap(), starts);
    assert_eq!(service.snapshot.audio_token, token);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 0);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}
