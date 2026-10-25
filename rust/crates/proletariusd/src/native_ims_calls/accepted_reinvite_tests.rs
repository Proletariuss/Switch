static ACCEPTED_REINVITE_BRANCH: AtomicUsize = AtomicUsize::new(0);

fn ar_invite(call_id: &str, local_tag: &str, seq: u32, body: Vec<u8>) -> Request {
    let mut request = p5_offer_request(call_id, local_tag, seq, Method::INVITE, body);
    let branch = ACCEPTED_REINVITE_BRANCH.fetch_add(1, Ordering::SeqCst);
    request.data.headers.set(
        Header::new(
            "Via",
            format!("SIP/2.0/TCP peer.ims.test;branch=z9hG4bKar{branch};rport"),
        )
        .unwrap(),
    );
    request
}

fn ar_wb(call_id: &str, local_tag: &str, seq: u32, port: u16) -> Request {
    ar_invite(
        call_id,
        local_tag,
        seq,
        p5_wb_offer(111, port, "octet-align=1;mode-set=2,5"),
    )
}

fn ar_ack(call_id: &str, local_tag: &str, seq: u32) -> Request {
    in_dialog(call_id, seq, Method::ACK, local_tag)
}

fn ar_cancel(invite: &Request) -> Request {
    let mut cancel = invite.clone();
    cancel.method = Method::CANCEL;
    cancel.data.body.clear();
    cancel.data.headers.set(
        Header::new(
            "CSeq",
            format!("{} CANCEL", cseq(&invite.data.headers).unwrap().0),
        )
        .unwrap(),
    );
    cancel
}

fn ar_record<'a>(service: &'a Service, call_id: &str, seq: u32) -> &'a AcceptedReinvite {
    service
        .call
        .iter()
        .chain(service.retired.iter())
        .find(|call| call.binding.call_id == call_id)
        .unwrap()
        .accepted_reinvites
        .iter()
        .find(|record| record.cseq == seq)
        .unwrap()
}

fn ar_schedule(service: &Service, call_id: &str, seq: u32) -> (Instant, Option<Instant>, Duration) {
    let record = ar_record(service, call_id, seq);
    (
        record.deadline,
        record.next_response_retry,
        record.response_retry_interval,
    )
}

async fn ar_accept(
    service: &mut Service,
    wire: &mut mpsc::UnboundedReceiver<Wire>,
    invite: &Request,
) -> Response {
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    let accepted = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(accepted.status, 200);
    accepted
}

fn ar_offerless_answer(
    call_id: &str,
    local_tag: &str,
    seq: u32,
    response: &Response,
    port: u16,
) -> Request {
    let answer = String::from_utf8(response.data.body.clone())
        .unwrap()
        .replace("192.0.2.10", "192.0.2.20")
        .replace("m=audio 49170", &format!("m=audio {port}"));
    p5_offer_request(call_id, local_tag, seq, Method::ACK, answer.into_bytes())
}

fn ar_failed_bye_writer_fixture(
    fail_200: bool,
) -> (
    Service,
    Arc<FakeMedia>,
    mpsc::UnboundedReceiver<Wire>,
    tokio::task::JoinHandle<()>,
) {
    let (commands, mut input) = mpsc::channel(32);
    let (sent, output) = mpsc::unbounded_channel();
    let media = Arc::new(FakeMedia::default());
    let writer_media = media.clone();
    let task = tokio::spawn(async move {
        while let Some(command) = input.recv().await {
            match command {
                Command::SendUntracked { request, reply, .. } => {
                    let fail = request.method == Method::BYE;
                    sent.send(Wire {
                        request: Some(request),
                        response: None,
                        connection: SipConnectionId::Primary,
                    })
                    .unwrap();
                    let _ = reply.send(if fail {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "controlled cleanup BYE failure",
                        ))
                    } else {
                        Ok(())
                    });
                }
                Command::RespondDialog {
                    connection,
                    response,
                    reply,
                    ..
                } => {
                    let fail = fail_200 && response.status == 200;
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
                    let _ = reply.send(if fail {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "controlled first 200 failure",
                        ))
                    } else {
                        Ok(())
                    });
                }
                _ => panic!("voice must not use tracked SMS transactions"),
            }
        }
    });
    let service = Service::new(g(), config(), commands, media.clone()).unwrap();
    (service, media, output, task)
}

#[tokio::test]
async fn accepted_reinvite_multiple_exchanges_keep_old_identity_without_replaying_or_recommitting()
{
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let started_at = service.snapshot.started_at.clone();
    let first = ar_wb(&call_id, &local_tag, 2, 49200);
    ar_accept(&mut service, &mut wire, &first).await;
    service
        .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
        .await
        .unwrap();
    let second = ar_invite(&call_id, &local_tag, 3, p5_evs_offer(96, 49202));
    ar_accept(&mut service, &mut wire, &second).await;
    service
        .handle_event(&request_event(ar_ack(&call_id, &local_tag, 3)))
        .await
        .unwrap();
    let third = ar_wb(&call_id, &local_tag, 4, 49204);
    let answer = ar_accept(&mut service, &mut wire, &third).await;
    let newest_schedule = ar_schedule(&service, &call_id, 4);
    let oldest_schedule = ar_schedule(&service, &call_id, 2);
    let prepared = media.prepared.lock().unwrap().len();
    let configured = media.configured.lock().unwrap().len();
    assert_eq!(service.call.as_ref().unwrap().accepted_reinvites.len(), 3);
    assert_eq!(service.snapshot.codec, "AMR-WB");
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .remote
            .as_ref()
            .unwrap()
            .remote_port,
        49202
    );
    for old in [&first, &second, &third] {
        service
            .handle_event(&request_event(old.clone()))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
    }
    service
        .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
        .await
        .unwrap();
    service
        .handle_event(&request_event(ar_ack(&call_id, &local_tag, 3)))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(ar_schedule(&service, &call_id, 2), oldest_schedule);
    assert_eq!(ar_schedule(&service, &call_id, 4), newest_schedule);
    assert!(!ar_record(&service, &call_id, 4).acked);
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .pending_reinvite
            .as_ref()
            .unwrap()
            .cseq,
        4
    );
    service
        .handle_event(&request_event(ar_cancel(&first)))
        .await
        .unwrap();
    let cancelled = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(cancelled.status, 200);
    assert_eq!(cseq(&cancelled.data.headers).unwrap().1, Method::CANCEL);
    assert_eq!(ar_schedule(&service, &call_id, 4), newest_schedule);
    service.tick(g(), newest_schedule.1.unwrap()).await.unwrap();
    assert_eq!(
        initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
        initial_response_bytes(&answer)
    );
    service
        .handle_event(&request_event(ar_ack(&call_id, &local_tag, 4)))
        .await
        .unwrap();
    assert!(ar_record(&service, &call_id, 4).acked);
    assert!(
        ar_record(&service, &call_id, 4)
            .next_response_retry
            .is_none()
    );
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 4);
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .remote
            .as_ref()
            .unwrap()
            .remote_port,
        49204
    );
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    assert_eq!(media.started.load(Ordering::SeqCst), 1);
    assert_eq!(service.snapshot.started_at, started_at);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_only_matching_ack_stops_tu_and_timer_l_outlives_ack() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let invite = ar_wb(&call_id, &local_tag, 2, 49210);
    let answer = ar_accept(&mut service, &mut wire, &invite).await;
    let schedule = ar_schedule(&service, &call_id, 2);
    for event in [
        initial_request_on(
            ar_ack(&call_id, &local_tag, 2),
            SipConnectionId::Incoming(99),
        ),
        request_event(ar_ack(&call_id, "unrelated", 2)),
        request_event(ar_ack(&call_id, &local_tag, 77)),
    ] {
        service.handle_event(&event).await.unwrap();
    }
    for _ in 0..2 {
        service
            .handle_event(&request_event(invite.clone()))
            .await
            .unwrap();
    }
    assert!(wire.try_recv().is_err());
    assert_eq!(ar_schedule(&service, &call_id, 2), schedule);
    assert!(!ar_record(&service, &call_id, 2).acked);
    service
        .tick(g(), schedule.1.unwrap() - Duration::from_nanos(1))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    service.tick(g(), schedule.1.unwrap()).await.unwrap();
    assert_eq!(
        initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
        initial_response_bytes(&answer)
    );
    assert_eq!(
        ar_record(&service, &call_id, 2).response_retry_interval,
        Duration::from_secs(1)
    );
    service
        .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
        .await
        .unwrap();
    assert!(ar_record(&service, &call_id, 2).acked);
    assert!(
        ar_record(&service, &call_id, 2)
            .next_response_retry
            .is_none()
    );
    assert_eq!(ar_record(&service, &call_id, 2).deadline, schedule.0);
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(wire.try_recv().is_err());
    service.tick(g(), schedule.0).await.unwrap();
    assert!(service.call.as_ref().unwrap().accepted_reinvites.is_empty());
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_held_first_200_does_not_commit_before_receipt() {
    let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let configured = media.configured.lock().unwrap().len();
    let invite = ar_wb(&call_id, &local_tag, 2, 49212);
    let event = request_event(invite.clone());
    let mut handling = Box::pin(service.handle_event(&event));
    tokio::select! {
        result = &mut handling => panic!("answer completed before writer receipt: {}", result.is_ok()),
        observed = wire.recv() => assert_eq!(observed.unwrap().response.unwrap().status, 200),
    }
    assert_eq!(media.prepared.lock().unwrap().len(), 1);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let receipt_at = Instant::now();
    receipts.recv().await.unwrap().send(Ok(())).unwrap();
    handling.await.unwrap();
    let accepted_at = Instant::now();
    let record = ar_record(&service, &call_id, 2);
    assert!(record.confirmed && !record.acked);
    assert!(record.deadline >= receipt_at + Duration::from_secs(32));
    assert!(record.deadline <= accepted_at + Duration::from_secs(32));
    assert!(record.next_response_retry.unwrap() >= receipt_at + Duration::from_millis(500));
    assert_eq!(media.configured.lock().unwrap().len(), configured + 1);
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_first_write_unknown_keeps_identity_and_cleans_once_after_ack_or_timeout()
{
    for discard_fails in [false, true] {
        for acknowledge in [false, true] {
            let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
            let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
            let configured = media.configured.lock().unwrap().len();
            media.discard_fail.store(discard_fails, Ordering::SeqCst);
            let mut invite = ar_wb(&call_id, &local_tag, 2, 49214);
            invite
                .data
                .headers
                .set(Header::new("Contact", "<sip:b@unknown-contact.ims.test>").unwrap());
            let event = request_event(invite.clone());
            let mut handling = Box::pin(service.handle_event(&event));
            tokio::select! {
                _ = &mut handling => panic!("unknown answer finished before receipt"),
                observed = wire.recv() => assert_eq!(observed.unwrap().response.unwrap().status, 200),
            }
            receipts
                .recv()
                .await
                .unwrap()
                .send(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "controlled reINVITE first 200 failure",
                )))
                .unwrap();
            assert!(handling.await.is_err());
            assert_eq!(service.snapshot.state, "failed");
            assert!(service.snapshot.uncertain);
            assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
            let record = ar_record(&service, &call_id, 2);
            assert!(!record.confirmed && !record.acked && record.cleanup_on_ack);
            assert!(record.next_response_retry.is_none());
            assert_eq!(
                record.cleanup_target.as_ref().unwrap().to_string(),
                "sip:b@unknown-contact.ims.test"
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
                "sip:b@peer.ims.test"
            );
            let deadline = record.deadline;
            service
                .handle_event(&request_event(invite.clone()))
                .await
                .unwrap();
            for event in [
                initial_request_on(
                    ar_ack(&call_id, &local_tag, 2),
                    SipConnectionId::Incoming(99),
                ),
                request_event(ar_ack(&call_id, "unrelated", 2)),
                request_event(ar_ack(&call_id, &local_tag, 77)),
            ] {
                service.handle_event(&event).await.unwrap();
            }
            assert!(wire.try_recv().is_err());
            if acknowledge {
                service
                    .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
                    .await
                    .unwrap();
            } else {
                service.tick(g(), deadline).await.unwrap();
            }
            let bye = wire.recv().await.unwrap().request.unwrap();
            assert_eq!(bye.method, Method::BYE);
            assert_eq!(single(&bye.data.headers, "Call-ID").unwrap(), call_id);
            assert_eq!(bye.uri.to_string(), "sip:b@unknown-contact.ims.test");
            for _ in 0..2 {
                service
                    .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
                    .await
                    .unwrap();
                service
                    .tick(g(), deadline + Duration::from_secs(1))
                    .await
                    .unwrap();
            }
            assert!(service.call.as_ref().unwrap().dialog_terminated);
            assert_eq!(service.snapshot.state, "failed");
            assert!(service.snapshot.uncertain);
            assert_eq!(media.configured.lock().unwrap().len(), configured);
            assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
            assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
            assert!(wire.try_recv().is_err());
            drop(service);
            task.await.unwrap();
        }
    }
}

#[tokio::test]
async fn accepted_reinvite_known_commit_failure_retains_transaction_and_stop_failure_fence() {
    for fail_stop in [false, true] {
        let (mut service, media, mut wire, task) = fixture();
        let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
        media.configure_fail.store(true, Ordering::SeqCst);
        media.stop_fail.store(fail_stop, Ordering::SeqCst);
        let mut invite = ar_wb(&call_id, &local_tag, 2, 49216);
        invite
            .data
            .headers
            .set(Header::new("Contact", "<sip:b@accepted-failure.ims.test>").unwrap());
        assert!(
            service
                .handle_event(&request_event(invite.clone()))
                .await
                .is_err()
        );
        let answer = wire.recv().await.unwrap().response.unwrap();
        assert_eq!(answer.status, 200);
        let bye = wire.recv().await.unwrap().request.unwrap();
        assert_eq!(bye.method, Method::BYE);
        assert_eq!(bye.uri.to_string(), "sip:b@accepted-failure.ims.test");
        assert!(service.call.as_ref().unwrap().dialog_terminated);
        assert!(ar_record(&service, &call_id, 2).confirmed);
        let deadline = ar_record(&service, &call_id, 2).deadline;
        let retry = ar_record(&service, &call_id, 2)
            .next_response_retry
            .unwrap();
        service.handle_event(&request_event(invite)).await.unwrap();
        assert!(wire.try_recv().is_err());
        service.tick(g(), retry).await.unwrap();
        assert_eq!(
            initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
            initial_response_bytes(&answer)
        );
        service
            .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
            .await
            .unwrap();
        service.tick(g(), deadline).await.unwrap();
        assert_eq!(service.snapshot.state, "failed");
        assert_eq!(service.snapshot.last_error, "ims_media_commit_failed");
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(service.call.as_ref().unwrap().media_allocated, fail_stop);
        if fail_stop {
            assert!(service.snapshot.uncertain && service.call.as_ref().unwrap().uncertain);
            assert!(service.dial(g(), "+447700900123").await.is_err());
            media.stop_fail.store(false, Ordering::SeqCst);
            service.shutdown(g()).await.unwrap();
            assert!(!service.call.as_ref().unwrap().media_allocated);
            assert!(service.snapshot.uncertain);
        }
        assert!(wire.try_recv().is_err());
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn accepted_reinvite_offerless_duplicate_ack_never_reapplies_or_invalidates_an_old_answer() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let prepared = media.prepared.lock().unwrap().len();
    let configured = media.configured.lock().unwrap().len();
    let invite = ar_invite(&call_id, &local_tag, 2, Vec::new());
    let offered = ar_accept(&mut service, &mut wire, &invite).await;
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    let ack = ar_offerless_answer(&call_id, &local_tag, 2, &offered, 49218);
    service.handle_event(&request_event(ack)).await.unwrap();
    assert!(ar_record(&service, &call_id, 2).acked);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared + 1);
    assert_eq!(media.configured.lock().unwrap().len(), configured + 1);
    let new_invite = ar_wb(&call_id, &local_tag, 3, 49220);
    ar_accept(&mut service, &mut wire, &new_invite).await;
    let prepared = media.prepared.lock().unwrap().len();
    let configured = media.configured.lock().unwrap().len();
    let schedule = ar_schedule(&service, &call_id, 3);
    let changed_old_ack = p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::ACK,
        p5_wb_offer(111, 49998, "octet-align=1;mode-set=2,5"),
    );
    for _ in 0..2 {
        service
            .handle_event(&request_event(changed_old_ack.clone()))
            .await
            .unwrap();
    }
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.snapshot.codec, "AMR-WB");
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .remote
            .as_ref()
            .unwrap()
            .remote_port,
        49218
    );
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .pending_reinvite
            .as_ref()
            .unwrap()
            .cseq,
        3
    );
    assert_eq!(ar_schedule(&service, &call_id, 3), schedule);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_retired_dialog_keeps_tu_but_old_requests_cannot_touch_new_call() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let invite = ar_wb(&call_id, &local_tag, 2, 49222);
    let answer = ar_accept(&mut service, &mut wire, &invite).await;
    let retry = ar_record(&service, &call_id, 2)
        .next_response_retry
        .unwrap();
    let deadline = ar_record(&service, &call_id, 2).deadline;
    let bye = in_dialog(&call_id, 3, Method::BYE, &local_tag);
    service
        .handle_event(&request_event(bye.clone()))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    service.dial(g(), "+447700900123").await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::INVITE
    );
    let current_id = service.snapshot.call_id.clone();
    let prepared = media.prepared.lock().unwrap().len();
    let configured = media.configured.lock().unwrap().len();
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(ar_record(&service, &call_id, 2).deadline, deadline);
    assert!(service.next_deadline().is_some_and(|at| at <= retry));
    service.tick(g(), retry).await.unwrap();
    assert_eq!(
        initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
        initial_response_bytes(&answer)
    );
    service
        .handle_event(&request_event(ar_cancel(&invite)))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    for _ in 0..2 {
        service
            .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
            .await
            .unwrap();
    }
    service.handle_event(&request_event(bye)).await.unwrap();
    assert!([200, 481].contains(&wire.recv().await.unwrap().response.unwrap().status));
    let unknown = ar_wb(&call_id, &local_tag, 4, 49224);
    service.handle_event(&request_event(unknown)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 481);
    service.tick(g(), deadline).await.unwrap();
    assert_eq!(service.snapshot.call_id, current_id);
    assert_eq!(service.snapshot.state, "outgoing");
    assert!(service.snapshot.last_error.is_empty());
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_capacity_preserves_all_live_records_and_cached_503_across_success() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let mut first = None;
    for index in 0..MAX_ACCEPTED_REINVITES {
        let seq = index as u32 + 2;
        let invite = ar_wb(&call_id, &local_tag, seq, 49300 + index as u16 * 2);
        ar_accept(&mut service, &mut wire, &invite).await;
        service
            .handle_event(&request_event(ar_ack(&call_id, &local_tag, seq)))
            .await
            .unwrap();
        if index == 0 {
            first = Some(invite);
        }
    }
    let prepared = media.prepared.lock().unwrap().len();
    let configured = media.configured.lock().unwrap().len();
    let blocked_seq = MAX_ACCEPTED_REINVITES as u32 + 2;
    let blocked = ar_wb(&call_id, &local_tag, blocked_seq, 49400);
    service
        .handle_event(&request_event(blocked.clone()))
        .await
        .unwrap();
    let rejected = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(rejected.status, 503);
    assert!(rejected.data.headers.get("Retry-After").is_some());
    service
        .handle_event(&request_event(blocked.clone()))
        .await
        .unwrap();
    assert_eq!(
        initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
        initial_response_bytes(&rejected)
    );
    let oldest_deadline = ar_record(&service, &call_id, 2).deadline;
    service
        .handle_event(&request_event(first.unwrap()))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(ar_record(&service, &call_id, 2).deadline, oldest_deadline);
    assert_eq!(
        service.call.as_ref().unwrap().accepted_reinvites.len(),
        MAX_ACCEPTED_REINVITES
    );
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    let expiry = service
        .call
        .as_ref()
        .unwrap()
        .accepted_reinvites
        .iter()
        .map(|record| record.deadline)
        .max()
        .unwrap();
    service.tick(g(), expiry).await.unwrap();
    assert!(service.call.as_ref().unwrap().accepted_reinvites.is_empty());
    let next = ar_wb(&call_id, &local_tag, blocked_seq + 1, 49402);
    ar_accept(&mut service, &mut wire, &next).await;
    service.handle_event(&request_event(blocked)).await.unwrap();
    assert_eq!(
        initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
        initial_response_bytes(&rejected)
    );
    assert_eq!(media.prepared.lock().unwrap().len(), prepared + 1);
    assert_eq!(media.configured.lock().unwrap().len(), configured + 1);
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_retired_capacity_does_not_evict_accepted_dialogs_to_admit_new_calls() {
    let (mut service, media, mut wire, task) = fixture();
    let mut oldest = None;
    for index in 0..=MAX_RETIRED {
        let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
        let invite = ar_wb(&call_id, &local_tag, 2, 49410 + index as u16 * 2);
        ar_accept(&mut service, &mut wire, &invite).await;
        if index == 0 {
            oldest = Some((call_id.clone(), invite));
        }
        service
            .handle_event(&request_event(in_dialog(
                &call_id,
                3,
                Method::BYE,
                &local_tag,
            )))
            .await
            .unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    }
    assert_eq!(service.retired.len(), MAX_RETIRED);
    let current_id = service.snapshot.call_id.clone();
    assert!(service.dial(g(), "+447700900123").await.is_err());
    service
        .handle_event(&request_event(inbound(
            "accepted-capacity-overflow",
            14,
            Method::INVITE,
            None,
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 486);
    assert!(wire.try_recv().is_err());
    let (oldest_id, oldest_invite) = oldest.unwrap();
    service
        .handle_event(&request_event(oldest_invite))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    assert!(!ar_record(&service, &oldest_id, 2).acked);
    assert_eq!(service.snapshot.call_id, current_id);
    let expiry = service
        .call
        .iter()
        .chain(service.retired.iter())
        .flat_map(|call| call.accepted_reinvites.iter())
        .map(|record| record.deadline)
        .max()
        .unwrap();
    service.tick(g(), expiry).await.unwrap();
    assert!(wire.try_recv().is_err());
    service.dial(g(), "+447700900123").await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::INVITE
    );
    assert_eq!(media.allocated.load(Ordering::SeqCst), MAX_RETIRED + 2);
    assert_eq!(media.stopped.load(Ordering::SeqCst), MAX_RETIRED + 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_shutdown_clears_current_and_retired_tables_even_when_stop_fails() {
    let (mut service, media, mut wire, task) = fixture();
    let (old_id, old_tag) = p5_establish(&mut service, &mut wire).await;
    ar_accept(&mut service, &mut wire, &ar_wb(&old_id, &old_tag, 2, 49450)).await;
    service
        .handle_event(&request_event(in_dialog(&old_id, 3, Method::BYE, &old_tag)))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    ar_accept(
        &mut service,
        &mut wire,
        &ar_wb(&call_id, &local_tag, 2, 49452),
    )
    .await;
    assert_eq!(service.retired[0].accepted_reinvites.len(), 1);
    media.stop_fail.store(true, Ordering::SeqCst);
    assert!(service.shutdown(g()).await.err().unwrap().uncertain);
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain && service.call.as_ref().unwrap().media_allocated);
    for call in service.call.iter().chain(service.retired.iter()) {
        assert!(call.accepted_reinvites.is_empty());
        assert!(call.pending_reinvite.is_none());
    }
    assert_eq!(service.next_deadline(), None);
    assert!(!service.snapshot.signaling_ready);
    assert!(wire.try_recv().is_err());
    media.stop_fail.store(false, Ordering::SeqCst);
    service.shutdown(g()).await.unwrap();
    assert!(!service.call.as_ref().unwrap().media_allocated);
    assert!(service.snapshot.uncertain);
    assert_eq!(service.next_deadline(), None);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_preparation_rejection_is_cached_without_an_accepted_slot_or_media_change()
 {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let remote = service.call.as_ref().unwrap().remote.clone();
    let configured = media.configured.lock().unwrap().len();
    media.prepare_fail.store(true, Ordering::SeqCst);
    let rejected_invite = ar_wb(&call_id, &local_tag, 2, 49454);
    service
        .handle_event(&request_event(rejected_invite.clone()))
        .await
        .unwrap();
    let rejected = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(rejected.status, 488);
    assert!(service.call.as_ref().unwrap().accepted_reinvites.is_empty());
    media.prepare_fail.store(false, Ordering::SeqCst);
    let accepted_invite = ar_wb(&call_id, &local_tag, 3, 49456);
    ar_accept(&mut service, &mut wire, &accepted_invite).await;
    service
        .handle_event(&request_event(ar_ack(&call_id, &local_tag, 3)))
        .await
        .unwrap();
    let prepared = media.prepared.lock().unwrap().len();
    for _ in 0..2 {
        service
            .handle_event(&request_event(rejected_invite.clone()))
            .await
            .unwrap();
        assert_eq!(
            initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
            initial_response_bytes(&rejected)
        );
    }
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured + 1);
    assert_ne!(service.call.as_ref().unwrap().remote, remote);
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .remote
            .as_ref()
            .unwrap()
            .remote_port,
        49456
    );
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_tu_write_failure_preserves_original_known_acceptance_and_cleans_once() {
    let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let invite = ar_wb(&call_id, &local_tag, 2, 49458);
    let event = request_event(invite.clone());
    let mut handling = Box::pin(service.handle_event(&event));
    let original = tokio::select! {
        _ = &mut handling => panic!("first 200 completed without receipt"),
        observed = wire.recv() => observed.unwrap().response.unwrap(),
    };
    receipts.recv().await.unwrap().send(Ok(())).unwrap();
    handling.await.unwrap();
    let schedule = ar_schedule(&service, &call_id, 2);
    let mut ticking = Box::pin(service.tick(g(), schedule.1.unwrap()));
    tokio::select! {
        _ = &mut ticking => panic!("TU response completed without receipt"),
        observed = wire.recv() => assert_eq!(initial_response_bytes(&observed.unwrap().response.unwrap()), initial_response_bytes(&original)),
    }
    receipts
        .recv()
        .await
        .unwrap()
        .send(Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "controlled TU response write failure",
        )))
        .unwrap();
    assert!(ticking.await.err().unwrap().uncertain);
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    let record = ar_record(&service, &call_id, 2);
    assert!(record.confirmed && !record.acked);
    assert_eq!(record.deadline, schedule.0);
    assert_eq!(
        initial_response_bytes(&record.response),
        initial_response_bytes(&original)
    );
    let prepared = media.prepared.lock().unwrap().len();
    let configured = media.configured.lock().unwrap().len();
    service.handle_event(&request_event(invite)).await.unwrap();
    service
        .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
        .await
        .unwrap();
    service.tick(g(), schedule.0).await.unwrap();
    let requests: Vec<_> = std::iter::from_fn(|| wire.try_recv().ok()).collect();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].request.as_ref().unwrap().method, Method::BYE);
    assert_eq!(
        single(
            &requests[0].request.as_ref().unwrap().data.headers,
            "Call-ID"
        )
        .unwrap(),
        call_id
    );
    service
        .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
        .await
        .unwrap();
    service
        .tick(g(), schedule.0 + Duration::from_secs(1))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_offerless_ack_prepare_failure_remains_acked_and_cannot_retry_media() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let invite = ar_invite(&call_id, &local_tag, 2, Vec::new());
    let offered = ar_accept(&mut service, &mut wire, &invite).await;
    let deadline = ar_record(&service, &call_id, 2).deadline;
    let configured = media.configured.lock().unwrap().len();
    media.prepare_fail.store(true, Ordering::SeqCst);
    let ack = ar_offerless_answer(&call_id, &local_tag, 2, &offered, 49460);
    assert!(
        service
            .handle_event(&request_event(ack.clone()))
            .await
            .is_err()
    );
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert!(ar_record(&service, &call_id, 2).acked);
    assert!(ar_record(&service, &call_id, 2).confirmed);
    assert!(
        ar_record(&service, &call_id, 2)
            .next_response_retry
            .is_none()
    );
    let attempts = media
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| *event == "prepare")
        .count();
    media.prepare_fail.store(false, Ordering::SeqCst);
    service.handle_event(&request_event(ack)).await.unwrap();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(wire.try_recv().is_err());
    service.tick(g(), deadline).await.unwrap();
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.call.as_ref().unwrap().dialog_terminated);
    assert_eq!(
        media
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| *event == "prepare")
            .count(),
        attempts
    );
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_on_incoming_dialog_uses_its_remote_tag_and_never_restarts_media_on_ack()
{
    let (mut service, media, mut wire, task) = fixture();
    let call_id = "accepted-incoming-origin";
    let (initial, local_tag, _) = initial_accept(&mut service, &mut wire, call_id).await;
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
    let starts = media.started.load(Ordering::SeqCst);
    let configured = media.configured.lock().unwrap().len();
    let mut invite = ar_wb(call_id, &local_tag, 15, 49464);
    invite
        .data
        .headers
        .set(Header::new("From", single(&initial.data.headers, "From").unwrap()).unwrap());
    ar_accept(&mut service, &mut wire, &invite).await;
    let schedule = ar_schedule(&service, call_id, 15);
    service.handle_event(&request_event(invite)).await.unwrap();
    service
        .handle_event(&request_event(ar_ack(call_id, &local_tag, 15)))
        .await
        .unwrap();
    assert!(!ar_record(&service, call_id, 15).acked);
    assert_eq!(ar_schedule(&service, call_id, 15), schedule);
    assert!(wire.try_recv().is_err());
    let mut ack = ar_ack(call_id, &local_tag, 15);
    ack.data
        .headers
        .set(Header::new("From", single(&initial.data.headers, "From").unwrap()).unwrap());
    for _ in 0..2 {
        service
            .handle_event(&request_event(ack.clone()))
            .await
            .unwrap();
    }
    assert!(ar_record(&service, call_id, 15).acked);
    assert!(
        ar_record(&service, call_id, 15)
            .next_response_retry
            .is_none()
    );
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.snapshot.codec, "AMR-WB");
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .remote
            .as_ref()
            .unwrap()
            .remote_port,
        49464
    );
    assert_eq!(media.started.load(Ordering::SeqCst), starts);
    assert_eq!(media.configured.lock().unwrap().len(), configured + 1);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_ack_timeout_marks_dialog_before_bye_and_never_repeats_after_stop_error()
{
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let invite = ar_wb(&call_id, &local_tag, 2, 49462);
    ar_accept(&mut service, &mut wire, &invite).await;
    let deadline = ar_record(&service, &call_id, 2).deadline;
    let configured = media.configured.lock().unwrap().len();
    media.stop_fail.store(true, Ordering::SeqCst);
    assert!(service.tick(g(), deadline).await.err().unwrap().uncertain);
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert!(service.call.as_ref().unwrap().dialog_terminated);
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_reinvite_ack_timeout");
    assert!(service.snapshot.uncertain && service.call.as_ref().unwrap().media_allocated);
    for _ in 0..2 {
        service
            .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
            .await
            .unwrap();
        service
            .tick(g(), deadline + Duration::from_secs(1))
            .await
            .unwrap();
    }
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    assert!(service.dial(g(), "+447700900123").await.is_err());
    assert!(wire.try_recv().is_err());
    media.stop_fail.store(false, Ordering::SeqCst);
    service.shutdown(g()).await.unwrap();
    assert!(!service.call.as_ref().unwrap().media_allocated);
    assert!(service.snapshot.uncertain);
    assert_eq!(service.next_deadline(), None);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn accepted_reinvite_cleanup_bye_write_error_still_finishes_live_media_on_ack_and_timeout() {
    for acknowledge in [true, false] {
        let (mut service, media, mut wire, task) = ar_failed_bye_writer_fixture(true);
        let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
        media.stop_fail.store(true, Ordering::SeqCst);
        let invite = ar_wb(&call_id, &local_tag, 2, 49466);
        assert!(service.handle_event(&request_event(invite)).await.is_err());
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
        assert_eq!(service.snapshot.state, "failed");
        assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
        assert!(service.snapshot.uncertain && service.call.as_ref().unwrap().media_allocated);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
        let deadline = ar_record(&service, &call_id, 2).deadline;
        media.stop_fail.store(false, Ordering::SeqCst);
        let result = if acknowledge {
            service
                .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
                .await
                .map(|_| ())
        } else {
            service.tick(g(), deadline).await
        };
        assert!(result.err().unwrap().uncertain);
        let bye = wire.recv().await.unwrap().request.unwrap();
        assert_eq!(bye.method, Method::BYE);
        assert_eq!(single(&bye.data.headers, "Call-ID").unwrap(), call_id);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        assert!(service.call.as_ref().unwrap().uncertain);
        assert!(service.call.as_ref().unwrap().dialog_terminated);
        assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
        assert_eq!(service.snapshot.state, "failed");
        assert!(service.snapshot.uncertain);
        assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
        for _ in 0..2 {
            service
                .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
                .await
                .unwrap();
            service
                .tick(g(), deadline + Duration::from_secs(1))
                .await
                .unwrap();
        }
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(wire.try_recv().is_err());
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn accepted_reinvite_live_ack_timeout_bye_write_failure_still_stops_active_media() {
    let (mut service, media, mut wire, task) = ar_failed_bye_writer_fixture(false);
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    ar_accept(
        &mut service,
        &mut wire,
        &ar_wb(&call_id, &local_tag, 2, 49468),
    )
    .await;
    assert_eq!(service.snapshot.state, "active");
    assert!(service.call.as_ref().unwrap().media_allocated);
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_some());
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    let deadline = ar_record(&service, &call_id, 2).deadline;
    let configured = media.configured.lock().unwrap().len();
    assert!(service.tick(g(), deadline).await.err().unwrap().uncertain);
    let bye = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(bye.method, Method::BYE);
    assert_eq!(single(&bye.data.headers, "Call-ID").unwrap(), call_id);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
    assert!(service.snapshot.uncertain && service.call.as_ref().unwrap().uncertain);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    assert!(service.call.as_ref().unwrap().dialog_terminated);
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
    assert!(service.call.as_ref().unwrap().session_timer.is_none());
    for _ in 0..2 {
        service
            .handle_event(&request_event(ar_ack(&call_id, &local_tag, 2)))
            .await
            .unwrap();
        service
            .tick(g(), deadline + Duration::from_secs(1))
            .await
            .unwrap();
    }
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}
