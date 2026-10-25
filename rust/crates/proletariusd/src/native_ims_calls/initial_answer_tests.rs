fn initial_response_bytes(response: &Response) -> Vec<u8> {
    proletarius_sip::Message::Response(response.clone()).encode()
}

fn initial_pending_schedule(call: &Call) -> Option<(Instant, Instant, Duration)> {
    call.initial_answer.as_ref().map(|pending| {
        (
            pending.deadline,
            pending.next_response_retry,
            pending.response_retry_interval,
        )
    })
}

fn initial_request_on(request: Request, connection: SipConnectionId) -> Event {
    let mut event = request_event(request);
    let RegisteredSipEvent::Message {
        connection: actual, ..
    } = &mut event.event
    else {
        unreachable!()
    };
    *actual = connection;
    event
}

async fn initial_accept(
    service: &mut Service,
    wire: &mut mpsc::UnboundedReceiver<Wire>,
    call_id: &str,
) -> (Request, String, Response) {
    let invite = inbound(call_id, 14, Method::INVITE, None);
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 180);
    service.answer(g()).await.unwrap();
    let answer = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(answer.status, 200);
    let local_tag = tag(answer.data.headers.get("To").unwrap().value())
        .unwrap()
        .to_owned();
    (invite, local_tag, answer)
}

#[tokio::test]
async fn initial_answer_retransmits_full_response_with_t1_backoff_and_t2_cap_on_tcp() {
    let (mut service, media, mut wire, task) = fixture();
    let (invite, _, answer) = initial_accept(&mut service, &mut wire, "initial-retry").await;
    assert!(
        invite
            .data
            .headers
            .get("Via")
            .unwrap()
            .value()
            .contains("TCP")
    );
    assert_eq!(media.prepared.lock().unwrap().len(), 1);
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    let original = initial_response_bytes(&answer);
    let mut prior_retry = None;
    for interval in [1000, 2000, 4000, 4000, 4000] {
        let pending = service
            .call
            .as_ref()
            .unwrap()
            .initial_answer
            .as_ref()
            .unwrap();
        let retry_at = pending.next_response_retry;
        if let Some((previous, previous_interval)) = prior_retry {
            assert_eq!(
                retry_at.duration_since(previous),
                Duration::from_millis(previous_interval)
            );
        }
        service
            .tick(g(), retry_at - Duration::from_nanos(1))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
        service.tick(g(), retry_at).await.unwrap();
        let retransmission = wire.recv().await.unwrap();
        assert_eq!(retransmission.connection, SipConnectionId::Primary);
        assert_eq!(
            initial_response_bytes(&retransmission.response.unwrap()),
            original
        );
        let pending = service
            .call
            .as_ref()
            .unwrap()
            .initial_answer
            .as_ref()
            .unwrap();
        assert_eq!(
            pending.response_retry_interval,
            Duration::from_millis(interval)
        );
        assert_eq!(
            pending.next_response_retry.duration_since(retry_at),
            Duration::from_millis(interval)
        );
        prior_retry = Some((retry_at, interval));
    }
    let schedule = initial_pending_schedule(service.call.as_ref().unwrap());
    let cache_deadline = service.call.as_ref().unwrap().initial_reply_deadline;
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(
        initial_pending_schedule(service.call.as_ref().unwrap()),
        schedule
    );
    assert_eq!(
        service.call.as_ref().unwrap().initial_reply_deadline,
        cache_deadline
    );
    service.tick(g(), schedule.unwrap().1).await.unwrap();
    assert_eq!(
        initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
        original
    );
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    assert_eq!(media.prepared.lock().unwrap().len(), 1);
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_answer_prepares_before_write_and_starts_rx_and_retry_clock_after_receipt() {
    let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
    service
        .handle_event(&request_event(inbound(
            "initial-held-receipt",
            14,
            Method::INVITE,
            None,
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 180);
    let mut answering = Box::pin(service.answer(g()));
    tokio::select! {
        _ = &mut answering => panic!("initial answer finished without writer receipt"),
        observed = wire.recv() => assert_eq!(observed.unwrap().response.unwrap().status, 200),
    }
    assert_eq!(media.prepared.lock().unwrap().len(), 1);
    assert!(media.configured.lock().unwrap().is_empty());
    assert!(media.start_modes.lock().unwrap().is_empty());
    tokio::time::sleep(Duration::from_millis(30)).await;
    let receipt_at = Instant::now();
    receipts.recv().await.unwrap().send(Ok(())).unwrap();
    answering.await.unwrap();
    let accepted_at = Instant::now();
    let pending = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap();
    assert!(pending.next_response_retry >= receipt_at + Duration::from_millis(500));
    assert!(pending.next_response_retry <= accepted_at + Duration::from_millis(500));
    assert!(pending.deadline >= receipt_at + Duration::from_secs(32));
    assert!(pending.deadline <= accepted_at + Duration::from_secs(32));
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    assert_eq!(service.snapshot.state, "connecting");
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
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_matching_ack_stops_retries_and_enables_transmit_without_reconfiguring() {
    let (mut service, media, mut wire, task) = fixture();
    let (invite, local_tag, answer) = initial_accept(&mut service, &mut wire, "initial-ack").await;
    let first_retry = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap()
        .next_response_retry;
    let ack = inbound("initial-ack", 14, Method::ACK, Some(&local_tag));
    let mut wrong_tag = ack.clone();
    wrong_tag
        .data
        .headers
        .set(Header::new("To", "<sip:a@ims.test>;tag=unrelated").unwrap());
    let wrong_cseq = inbound("initial-ack", 15, Method::ACK, Some(&local_tag));
    for event in [
        request_event(wrong_tag),
        request_event(wrong_cseq),
        initial_request_on(ack.clone(), SipConnectionId::Incoming(99)),
    ] {
        service.handle_event(&event).await.unwrap();
        assert_eq!(service.snapshot.state, "connecting");
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    }
    service.tick(g(), first_retry).await.unwrap();
    assert_eq!(
        initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
        initial_response_bytes(&answer)
    );
    service
        .handle_event(&request_event(ack.clone()))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "active");
    let started_at = service.snapshot.started_at.clone();
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true, false]);
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    for _ in 0..2 {
        service
            .handle_event(&request_event(ack.clone()))
            .await
            .unwrap();
    }
    let cache_deadline = service.call.as_ref().unwrap().initial_reply_deadline;
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(wire.try_recv().is_err());
    assert!(service.call.as_ref().unwrap().initial_answer.is_none());
    assert_eq!(
        service.call.as_ref().unwrap().initial_reply_deadline,
        cache_deadline
    );
    service
        .tick(g(), first_retry + Duration::from_secs(40))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.snapshot.started_at, started_at);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true, false]);
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_answer_ack_timeout_sends_one_bye_and_drains_without_ack_revival() {
    let (mut service, media, mut wire, task) = fixture();
    let (_, local_tag, _) = initial_accept(&mut service, &mut wire, "initial-timeout").await;
    let pending = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap();
    let deadline = pending.deadline;
    assert_eq!(pending.response_retry_interval, Duration::from_millis(500));
    assert_eq!(
        service.call.as_ref().unwrap().initial_reply_deadline,
        Some(deadline)
    );
    service.tick(g(), deadline).await.unwrap();
    let bye = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(bye.method, Method::BYE);
    assert_eq!(bye.uri.to_string(), "sip:b@peer.ims.test");
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_initial_ack_timeout");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    assert!(service.call.as_ref().unwrap().initial_answer.is_none());
    assert!(service.call.as_ref().unwrap().session_timer.is_none());
    for _ in 0..2 {
        service
            .handle_event(&request_event(inbound(
                "initial-timeout",
                14,
                Method::ACK,
                Some(&local_tag),
            )))
            .await
            .unwrap();
        service
            .tick(g(), deadline + Duration::from_secs(60))
            .await
            .unwrap();
    }
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_hangup_defers_bye_until_ack_and_never_activates_transmit() {
    let (mut service, media, mut wire, task) = fixture();
    let (_, local_tag, answer) = initial_accept(&mut service, &mut wire, "initial-hangup").await;
    service.hangup(g()).await.unwrap();
    service.hangup(g()).await.unwrap();
    assert_eq!(service.snapshot.state, "cancelling");
    assert!(media.transmit_held.load(Ordering::SeqCst));
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    assert!(wire.try_recv().is_err());
    let pending = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap();
    assert!(pending.hangup_requested);
    let retry_at = pending.next_response_retry;
    let deadline = pending.deadline;
    service.tick(g(), retry_at).await.unwrap();
    assert_eq!(
        initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
        initial_response_bytes(&answer)
    );
    let ack = inbound("initial-hangup", 14, Method::ACK, Some(&local_tag));
    service
        .handle_event(&request_event(ack.clone()))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "ended");
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service.handle_event(&request_event(ack)).await.unwrap();
    service.tick(g(), deadline).await.unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_hangup_without_ack_waits_for_server_timeout_before_bye() {
    let (mut service, media, mut wire, task) = fixture();
    initial_accept(&mut service, &mut wire, "initial-hangup-timeout").await;
    service.hangup(g()).await.unwrap();
    let deadline = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap()
        .deadline;
    assert!(wire.try_recv().is_err());
    service.tick(g(), deadline).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "ended");
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service
        .tick(g(), deadline + Duration::from_secs(60))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_late_cancel_returns_200_without_cancelling_accepted_answer() {
    let (mut service, media, mut wire, task) = fixture();
    let (_, _, answer) = initial_accept(&mut service, &mut wire, "initial-late-cancel").await;
    let cancel = inbound("initial-late-cancel", 14, Method::CANCEL, None);
    service
        .handle_event(&initial_request_on(
            cancel.clone(),
            SipConnectionId::Incoming(77),
        ))
        .await
        .unwrap();
    let rejected = wire.recv().await.unwrap();
    assert_eq!(rejected.connection, SipConnectionId::Incoming(77));
    assert_eq!(rejected.response.unwrap().status, 481);
    for _ in 0..2 {
        service
            .handle_event(&request_event(cancel.clone()))
            .await
            .unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
        assert_eq!(service.snapshot.state, "connecting");
        assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
        assert!(wire.try_recv().is_err());
    }
    let retry_at = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap()
        .next_response_retry;
    service.tick(g(), retry_at).await.unwrap();
    assert_eq!(
        initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
        initial_response_bytes(&answer)
    );
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_remote_bye_before_ack_drains_and_late_ack_cannot_start_session() {
    let (mut service, media, mut wire, task) = fixture();
    let (_, local_tag, _) = initial_accept(&mut service, &mut wire, "initial-remote-bye").await;
    let deadline = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap()
        .deadline;
    service
        .handle_event(&request_event(inbound(
            "initial-remote-bye",
            15,
            Method::BYE,
            Some(&local_tag),
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    assert_eq!(service.snapshot.state, "ended");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    for _ in 0..2 {
        service
            .handle_event(&request_event(inbound(
                "initial-remote-bye",
                14,
                Method::ACK,
                Some(&local_tag),
            )))
            .await
            .unwrap();
    }
    service
        .tick(g(), deadline + Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "ended");
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    while let Ok(message) = wire.try_recv() {
        assert!(
            message.request.is_none(),
            "terminated dialog must not send another BYE"
        );
    }
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_unknown_answer_write_fences_media_and_absorbs_duplicate_invite() {
    let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
    let invite = inbound("initial-unknown-write", 14, Method::INVITE, None);
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 180);
    let local_tag = service.call.as_ref().unwrap().local_tag.clone();
    let mut answering = Box::pin(service.answer(g()));
    tokio::select! {
        _ = &mut answering => panic!("initial answer finished without writer receipt"),
        observed = wire.recv() => assert_eq!(observed.unwrap().response.unwrap().status, 200),
    }
    assert_eq!(media.prepared.lock().unwrap().len(), 1);
    assert!(media.configured.lock().unwrap().is_empty());
    assert!(media.start_modes.lock().unwrap().is_empty());
    receipts
        .recv()
        .await
        .unwrap()
        .send(Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "controlled initial 200 failure",
        )))
        .unwrap();
    assert!(answering.await.err().unwrap().uncertain);
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    for event in [
        request_event(inbound(
            "initial-unknown-write",
            15,
            Method::ACK,
            Some(&local_tag),
        )),
        request_event(inbound(
            "initial-unknown-write",
            14,
            Method::ACK,
            Some("unrelated"),
        )),
        initial_request_on(
            inbound("initial-unknown-write", 14, Method::ACK, Some(&local_tag)),
            SipConnectionId::Incoming(99),
        ),
    ] {
        service.handle_event(&event).await.unwrap();
    }
    assert!(wire.try_recv().is_err());
    service
        .handle_event(&request_event(inbound(
            "initial-unknown-write",
            14,
            Method::ACK,
            Some(&local_tag),
        )))
        .await
        .unwrap();
    let bye = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(bye.method, Method::BYE);
    assert_eq!(
        single(&bye.data.headers, "Call-ID").unwrap(),
        "initial-unknown-write"
    );
    for _ in 0..2 {
        service
            .handle_event(&request_event(invite.clone()))
            .await
            .unwrap();
        service
            .handle_event(&request_event(inbound(
                "initial-unknown-write",
                14,
                Method::ACK,
                Some(&local_tag),
            )))
            .await
            .unwrap();
    }
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert!(service.call.as_ref().unwrap().uncertain);
    assert!(!service.call.as_ref().unwrap().initial_reply_confirmed);
    assert!(wire.try_recv().is_err());
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    assert!(media.configured.lock().unwrap().is_empty());
    assert!(media.start_modes.lock().unwrap().is_empty());
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

async fn initial_fail_after_accepted_commit(
    service: &mut Service,
    media: &Arc<FakeMedia>,
    wire: &mut mpsc::UnboundedReceiver<Wire>,
    call_id: &str,
) -> String {
    service
        .handle_event(&request_event(inbound(call_id, 14, Method::INVITE, None)))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 180);
    media.configure_fail.store(true, Ordering::SeqCst);
    assert_eq!(
        service.answer(g()).await.err().unwrap().code,
        "ims_media_commit_failed"
    );
    let accepted = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(accepted.status, 200);
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_media_commit_failed");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(media.start_modes.lock().unwrap().is_empty());
    assert!(
        service
            .call
            .as_ref()
            .unwrap()
            .initial_answer
            .as_ref()
            .unwrap()
            .hangup_requested
    );
    assert!(wire.try_recv().is_err());
    media.configure_fail.store(false, Ordering::SeqCst);
    tag(accepted.data.headers.get("To").unwrap().value())
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn initial_accepted_media_failure_defers_one_bye_until_ack_or_server_timeout() {
    for acknowledge in [true, false] {
        let (mut service, media, mut wire, task) = fixture();
        let call_id = "initial-accepted-media-failed";
        let local_tag =
            initial_fail_after_accepted_commit(&mut service, &media, &mut wire, call_id).await;
        let deadline = service
            .call
            .as_ref()
            .unwrap()
            .initial_answer
            .as_ref()
            .unwrap()
            .deadline;
        if acknowledge {
            service
                .handle_event(&request_event(inbound(
                    call_id,
                    14,
                    Method::ACK,
                    Some(&local_tag),
                )))
                .await
                .unwrap();
        } else {
            service.tick(g(), deadline).await.unwrap();
        }
        let bye = wire.recv().await.unwrap().request.unwrap();
        assert_eq!(bye.method, Method::BYE);
        assert_eq!(single(&bye.data.headers, "Call-ID").unwrap(), call_id);
        assert_eq!(service.snapshot.state, "failed");
        assert_eq!(service.snapshot.last_error, "ims_media_commit_failed");
        for _ in 0..2 {
            service
                .handle_event(&request_event(inbound(
                    call_id,
                    14,
                    Method::ACK,
                    Some(&local_tag),
                )))
                .await
                .unwrap();
            service
                .tick(g(), deadline + Duration::from_secs(1))
                .await
                .unwrap();
        }
        assert!(wire.try_recv().is_err());
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
        assert!(media.start_modes.lock().unwrap().is_empty());
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_retired_media_failure_sends_old_bye_without_touching_new_call() {
    for acknowledge in [true, false] {
        let (mut service, media, mut wire, task) = fixture();
        let call_id = "initial-retired-media-failed";
        let local_tag =
            initial_fail_after_accepted_commit(&mut service, &media, &mut wire, call_id).await;
        let deadline = service
            .call
            .as_ref()
            .unwrap()
            .initial_answer
            .as_ref()
            .unwrap()
            .deadline;
        service.dial(g(), "+447700900123").await.unwrap();
        assert_eq!(
            wire.recv().await.unwrap().request.unwrap().method,
            Method::INVITE
        );
        let current_id = service.snapshot.call_id.clone();
        if acknowledge {
            service
                .handle_event(&request_event(inbound(
                    call_id,
                    14,
                    Method::ACK,
                    Some(&local_tag),
                )))
                .await
                .unwrap();
        } else {
            service.tick(g(), deadline).await.unwrap();
        }
        let bye = wire.recv().await.unwrap().request.unwrap();
        assert_eq!(bye.method, Method::BYE);
        assert_eq!(single(&bye.data.headers, "Call-ID").unwrap(), call_id);
        for _ in 0..2 {
            service
                .handle_event(&request_event(inbound(
                    call_id,
                    14,
                    Method::ACK,
                    Some(&local_tag),
                )))
                .await
                .unwrap();
            service
                .tick(g(), deadline + Duration::from_secs(1))
                .await
                .unwrap();
        }
        assert_eq!(service.snapshot.call_id, current_id);
        assert_eq!(service.snapshot.state, "outgoing");
        assert!(service.snapshot.last_error.is_empty());
        assert_eq!(media.allocated.load(Ordering::SeqCst), 2);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(media.start_modes.lock().unwrap().is_empty());
        assert!(wire.try_recv().is_err());
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_new_call_admission_cannot_prune_an_expired_deferred_bye_before_tick() {
    let (mut service, media, mut wire, task) = fixture();
    let call_id = "initial-admission-before-tick";
    initial_fail_after_accepted_commit(&mut service, &media, &mut wire, call_id).await;
    let expired = Instant::now() - Duration::from_millis(1);
    let call = service.call.as_mut().unwrap();
    call.initial_reply_deadline = Some(expired);
    call.initial_answer.as_mut().unwrap().deadline = expired;
    service.dial(g(), "+447700900123").await.unwrap();
    service.tick(g(), Instant::now()).await.unwrap();
    let mut methods = Vec::new();
    while let Ok(message) = wire.try_recv() {
        let request = message.request.unwrap();
        methods.push((
            request.method,
            single(&request.data.headers, "Call-ID").unwrap().to_owned(),
        ));
    }
    assert_eq!(
        methods
            .iter()
            .filter(|(method, id)| *method == Method::BYE && id == call_id)
            .count(),
        1
    );
    assert_eq!(
        methods
            .iter()
            .filter(|(method, _)| *method == Method::INVITE)
            .count(),
        1
    );
    assert_eq!(methods.len(), 2);
    assert_eq!(service.snapshot.state, "outgoing");
    assert_ne!(service.snapshot.call_id, call_id);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_expired_ack_after_remote_bye_cannot_send_another_bye_or_fail_ended_call() {
    let (mut service, media, mut wire, task) = fixture();
    let call_id = "initial-ended-expired-ack";
    let (_, local_tag, _) = initial_accept(&mut service, &mut wire, call_id).await;
    service
        .handle_event(&request_event(inbound(
            call_id,
            15,
            Method::BYE,
            Some(&local_tag),
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    let expired = Instant::now() - Duration::from_millis(1);
    let call = service.call.as_mut().unwrap();
    call.initial_reply_deadline = Some(expired);
    call.initial_answer.as_mut().unwrap().deadline = expired;
    for _ in 0..2 {
        service
            .handle_event(&request_event(inbound(
                call_id,
                14,
                Method::ACK,
                Some(&local_tag),
            )))
            .await
            .unwrap();
    }
    service.tick(g(), Instant::now()).await.unwrap();
    assert_eq!(service.snapshot.state, "ended");
    assert!(service.snapshot.last_error.is_empty());
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_shutdown_clears_current_and_retired_transactions_even_when_stop_fails() {
    for fail_stop in [false, true] {
        let (mut service, media, mut wire, task) = fixture();
        let (_, old_tag, _) =
            initial_accept(&mut service, &mut wire, "initial-shutdown-retired").await;
        service
            .handle_event(&request_event(inbound(
                "initial-shutdown-retired",
                15,
                Method::BYE,
                Some(&old_tag),
            )))
            .await
            .unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
        initial_accept(&mut service, &mut wire, "initial-shutdown-current").await;
        assert_eq!(service.retired.len(), 1);
        assert!(service.retired[0].initial_answer.is_some());
        assert!(service.call.as_ref().unwrap().initial_answer.is_some());
        let attempts = media.stop_attempts.load(Ordering::SeqCst);
        media.stop_fail.store(fail_stop, Ordering::SeqCst);
        let result = service.shutdown(g()).await;
        if fail_stop {
            assert!(result.err().unwrap().uncertain);
            assert_eq!(service.snapshot.state, "failed");
            assert!(service.snapshot.uncertain);
            assert!(service.call.as_ref().unwrap().uncertain);
            assert!(service.call.as_ref().unwrap().media_allocated);
        } else {
            result.unwrap();
            assert_eq!(service.snapshot.state, "ended");
            assert!(!service.call.as_ref().unwrap().media_allocated);
        }
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), attempts + 1);
        assert!(!service.snapshot.signaling_ready);
        assert!(!service.snapshot.media_ready);
        assert_eq!(service.next_deadline(), None);
        for call in service.call.iter().chain(service.retired.iter()) {
            assert!(call.initial_answer.is_none());
            assert!(call.initial_reply.is_none());
            assert!(call.initial_reply_deadline.is_none());
            assert!(!call.initial_reply_confirmed);
            assert!(call.deadline.is_none());
            assert!(call.pending_reinvite.is_none());
            assert!(call.session_timer.is_none());
        }
        assert!(wire.try_recv().is_err());
        if fail_stop {
            media.stop_fail.store(false, Ordering::SeqCst);
            service.shutdown(g()).await.unwrap();
            assert!(!service.call.as_ref().unwrap().media_allocated);
            assert!(service.call.as_ref().unwrap().uncertain);
            assert!(service.snapshot.uncertain);
            assert!(!service.snapshot.signaling_ready);
            assert_eq!(service.next_deadline(), None);
            assert_eq!(media.stop_attempts.load(Ordering::SeqCst), attempts + 2);
            assert!(wire.try_recv().is_err());
        }
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_receiver_prepare_failure_rejects_before_200_and_drains_allocation() {
    let (mut service, media, mut wire, task) = fixture();
    let invite = inbound("initial-prepare-failed", 14, Method::INVITE, None);
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 180);
    media.prepare_fail.store(true, Ordering::SeqCst);
    assert_eq!(
        service.answer(g()).await.err().unwrap().code,
        "ims_media_prepare_failed"
    );
    let rejected = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(rejected.status, 488);
    assert!(wire.try_recv().is_err());
    let events = media.events.lock().unwrap().clone();
    assert!(
        events.iter().position(|event| event == "prepare").unwrap()
            < events
                .iter()
                .position(|event| event == "response488")
                .unwrap()
    );
    assert!(!events.iter().any(|event| event == "response200"));
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    assert!(media.configured.lock().unwrap().is_empty());
    assert!(media.start_modes.lock().unwrap().is_empty());
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    for _ in 0..2 {
        service
            .handle_event(&request_event(invite.clone()))
            .await
            .unwrap();
        assert_eq!(
            initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
            initial_response_bytes(&rejected)
        );
    }
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(
        media
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| *event == "prepare")
            .count(),
        1
    );
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_accepted_transaction_survives_new_call_until_timer_l_without_ack_reactivation() {
    let (mut service, media, mut wire, task) = fixture();
    let call_id = "initial-retained";
    let (invite, local_tag, answer) = initial_accept(&mut service, &mut wire, call_id).await;
    service
        .handle_event(&request_event(inbound(
            call_id,
            15,
            Method::BYE,
            Some(&local_tag),
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    assert_eq!(service.snapshot.state, "ended");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service.dial(g(), "+447700900123").await.unwrap();
    let new_invite = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(new_invite.method, Method::INVITE);
    let current_id = service.snapshot.call_id.clone();
    assert_ne!(current_id, call_id);
    let deadline = service
        .retired
        .iter()
        .find(|call| call.binding.call_id == call_id)
        .unwrap()
        .initial_reply_deadline
        .unwrap();
    let configured = media.configured.lock().unwrap().len();
    let start_modes = media.start_modes.lock().unwrap().clone();
    let retry_at = service
        .retired
        .iter()
        .find(|call| call.binding.call_id == call_id)
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap()
        .next_response_retry;
    assert!(service.next_deadline().is_some_and(|at| at <= retry_at));
    service.tick(g(), retry_at).await.unwrap();
    assert_eq!(
        initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
        initial_response_bytes(&answer)
    );
    assert_eq!(service.snapshot.state, "outgoing");
    assert_eq!(service.snapshot.call_id, current_id);
    for _ in 0..2 {
        let schedule = initial_pending_schedule(
            service
                .retired
                .iter()
                .find(|call| call.binding.call_id == call_id)
                .unwrap(),
        );
        service
            .handle_event(&request_event(invite.clone()))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
        let retained = service
            .retired
            .iter()
            .find(|call| call.binding.call_id == call_id)
            .unwrap();
        assert_eq!(retained.initial_reply_deadline, Some(deadline));
        assert_eq!(initial_pending_schedule(retained), schedule);
        service
            .handle_event(&request_event(inbound(
                call_id,
                14,
                Method::ACK,
                Some(&local_tag),
            )))
            .await
            .unwrap();
        assert_eq!(service.snapshot.state, "outgoing");
        assert_eq!(service.snapshot.call_id, current_id);
        assert_eq!(service.call.as_ref().unwrap().binding.call_id, current_id);
    }
    assert!(
        service
            .retired
            .iter()
            .find(|call| call.binding.call_id == call_id)
            .unwrap()
            .initial_answer
            .is_none()
    );
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    assert_eq!(*media.start_modes.lock().unwrap(), start_modes);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(wire.try_recv().is_err());
    assert!(service.next_deadline().is_some_and(|at| at <= deadline));
    service.tick(g(), deadline).await.unwrap();
    assert!(
        service
            .retired
            .iter()
            .all(|call| call.binding.call_id != call_id || call.initial_reply.is_none())
    );
    assert_eq!(service.snapshot.call_id, current_id);
    assert_eq!(service.snapshot.state, "outgoing");
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 486);
    assert_eq!(media.allocated.load(Ordering::SeqCst), 2);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_accepted_retention_budget_rejects_admission_without_evicting_live_transactions() {
    let (mut service, media, mut wire, task) = fixture();
    let mut oldest = None;
    for index in 0..=MAX_RETIRED {
        let call_id = format!("initial-budget-{index}");
        let (invite, local_tag, answer) = initial_accept(&mut service, &mut wire, &call_id).await;
        if index == 0 {
            oldest = Some((invite, answer));
        }
        service
            .handle_event(&request_event(inbound(
                &call_id,
                15,
                Method::BYE,
                Some(&local_tag),
            )))
            .await
            .unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
        assert_eq!(service.snapshot.state, "ended");
    }
    assert_eq!(service.retired.len(), MAX_RETIRED);
    let current_id = service.snapshot.call_id.clone();
    service
        .handle_event(&request_event(inbound(
            "initial-budget-overflow",
            14,
            Method::INVITE,
            None,
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 486);
    assert!(wire.try_recv().is_err());
    assert!(service.dial(g(), "+447700900123").await.is_err());
    assert!(wire.try_recv().is_err());
    assert_eq!(service.snapshot.call_id, current_id);
    assert_eq!(service.retired.len(), MAX_RETIRED);
    let (oldest_invite, oldest_answer) = oldest.unwrap();
    let oldest_id = single(&oldest_invite.data.headers, "Call-ID")
        .unwrap()
        .to_owned();
    let (oldest_cache_deadline, oldest_schedule) = {
        let retained = service
            .retired
            .iter()
            .find(|call| call.binding.call_id == oldest_id)
            .unwrap();
        (
            retained.initial_reply_deadline,
            initial_pending_schedule(retained),
        )
    };
    service
        .handle_event(&request_event(oldest_invite))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    let retained = service
        .retired
        .iter()
        .find(|call| call.binding.call_id == oldest_id)
        .unwrap();
    assert_eq!(retained.initial_reply_deadline, oldest_cache_deadline);
    assert_eq!(initial_pending_schedule(retained), oldest_schedule);
    assert_eq!(
        initial_response_bytes(retained.initial_reply.as_ref().unwrap()),
        initial_response_bytes(&oldest_answer)
    );
    service.tick(g(), oldest_schedule.unwrap().1).await.unwrap();
    assert_eq!(
        initial_response_bytes(&wire.recv().await.unwrap().response.unwrap()),
        initial_response_bytes(&oldest_answer)
    );
    assert!(wire.try_recv().is_err());
    assert_eq!(media.allocated.load(Ordering::SeqCst), MAX_RETIRED + 1);
    assert_eq!(media.stopped.load(Ordering::SeqCst), MAX_RETIRED + 1);
    let final_deadline = service
        .retired
        .iter()
        .chain(service.call.iter())
        .filter_map(|call| call.initial_reply_deadline)
        .max()
        .unwrap();
    service.tick(g(), final_deadline).await.unwrap();
    service
        .handle_event(&request_event(inbound(
            "initial-budget-recovered",
            14,
            Method::INVITE,
            None,
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 100);
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 180);
    assert_eq!(service.snapshot.call_id, "initial-budget-recovered");
    assert_eq!(service.snapshot.state, "incoming");
    assert!(matches!(
        &service.call.as_ref().unwrap().tracker,
        Tracker::Incoming(tracker) if tracker.phase() == IncomingCallPhase::Ringing
    ));
    assert_eq!(media.allocated.load(Ordering::SeqCst), MAX_RETIRED + 1);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}
