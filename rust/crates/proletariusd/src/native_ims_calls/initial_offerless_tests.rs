// Initial offerless INVITEs have a local offer in the final response. The
// accepted transaction, receive preparation, and ACK answer have separate jobs.
macro_rules! io_recv {
    ($wire:expr) => {
        tokio::time::timeout(Duration::from_secs(2), $wire.recv())
            .await
            .expect("expected SIP wire message within two seconds")
            .expect("SIP writer closed before the expected message")
    };
}

fn io_sdp_request(
    call_id: &str,
    local_tag: &str,
    seq: u32,
    method: Method,
    body: Vec<u8>,
) -> Request {
    // Initial inbound() uses remote, whereas outgoing in_dialog() uses remote-tag.
    let mut request = inbound(call_id, seq, method, Some(local_tag));
    request.data.body = body;
    push(&mut request.data.headers, "Content-Type", "application/sdp").unwrap();
    request
}
fn io_invite(call_id: &str) -> Request {
    let mut request = inbound(call_id, 14, Method::INVITE, None);
    request.data.body.clear();
    request
}

async fn io_ring(
    service: &mut Service,
    wire: &mut mpsc::UnboundedReceiver<Wire>,
    invite: &Request,
) -> String {
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    assert_eq!(io_recv!(wire).response.unwrap().status, 100);
    let ringing = io_recv!(wire).response.unwrap();
    assert_eq!(ringing.status, 180);
    assert!(ringing.data.body.is_empty());
    assert!(ringing.data.headers.get("RSeq").is_none());
    assert_eq!(service.snapshot.state, "incoming");
    assert!(service.call.as_ref().unwrap().remote.is_none());
    tag(single(&ringing.data.headers, "To").unwrap())
        .unwrap()
        .to_owned()
}

async fn io_accept(
    service: &mut Service,
    wire: &mut mpsc::UnboundedReceiver<Wire>,
    call_id: &str,
) -> (Request, String, Response) {
    let invite = io_invite(call_id);
    let local_tag = io_ring(service, wire, &invite).await;
    service.answer(g()).await.unwrap();
    let response = io_recv!(wire).response.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        single(&response.data.headers, "Content-Type").unwrap(),
        "application/sdp"
    );
    assert!(!response.data.body.is_empty());
    assert_eq!(service.snapshot.state, "connecting");
    assert!(service.call.as_ref().unwrap().remote.is_none());
    (invite, local_tag, response)
}

fn io_ack(call_id: &str, local_tag: &str, response: &Response, evs: bool) -> Request {
    let offered = std::str::from_utf8(&response.data.body).unwrap();
    let (_, modes) = proletarius_call::evs::available_audio_codecs(false);
    let chosen = sdp::negotiate_audio_offer_with_modes(offered, evs, &modes).unwrap();
    assert_eq!(matches!(chosen.codec, AudioCodec::Evs { .. }), evs);
    let answer = sdp::build_local_audio_answer_to_offer_version(
        569,
        1,
        "192.0.2.20".parse().unwrap(),
        49200,
        offered,
        &chosen,
        false,
    )
    .unwrap();
    io_sdp_request(call_id, local_tag, 14, Method::ACK, answer.into_bytes())
}

fn io_assert_no_transmit(media: &FakeMedia) {
    assert!(media.configured.lock().unwrap().is_empty());
    assert!(media.start_modes.lock().unwrap().is_empty());
    assert_eq!(media.started.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn initial_offerless_rings_without_remote_or_media_and_supported_only_keeps_plain_180() {
    for supported in [false, true] {
        let (mut service, media, mut wire, task) = fixture();
        let mut invite = io_invite("offerless-ringing");
        if supported {
            push(&mut invite.data.headers, "Supported", "timer, 100rel").unwrap();
        }
        io_ring(&mut service, &mut wire, &invite).await;
        assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
        assert!(media.prepared.lock().unwrap().is_empty());
        assert!(media.local_offers.lock().unwrap().is_empty());
        io_assert_no_transmit(&media);
        assert!(wire.try_recv().is_err());
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_require_100rel_prepares_reliable_ringing_with_or_without_offer() {
    for offerless in [false, true] {
        let (mut service, media, mut wire, task) = fixture();
        let mut invite = if offerless {
            io_invite("require-100rel")
        } else {
            inbound("require-100rel", 14, Method::INVITE, None)
        };
        push(&mut invite.data.headers, "Require", "100rel").unwrap();
        service.handle_event(&request_event(invite)).await.unwrap();
        assert_eq!(io_recv!(wire).response.unwrap().status, 100);
        let ringing = io_recv!(wire).response.unwrap();
        assert_eq!(ringing.status, 180);
        assert!(token(&ringing.data.headers, "Require", "100rel"));
        assert!(
            single(&ringing.data.headers, "RSeq")
                .unwrap()
                .parse::<u32>()
                .unwrap()
                > 0
        );
        assert!(!ringing.data.body.is_empty());
        assert!(ringing.data.headers.get("Unsupported").is_none());
        assert!(service.call.is_some());
        assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
        if offerless {
            assert_eq!(*media.local_offers.lock().unwrap(), vec![ringing.data.body]);
            io_assert_no_transmit(&media);
        } else {
            assert!(media.local_offers.lock().unwrap().is_empty());
            assert_eq!(media.prepared.lock().unwrap().len(), 1);
            assert_eq!(media.configured.lock().unwrap().len(), 1);
            assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
        }
        assert!(wire.try_recv().is_err());
        service.shutdown(g()).await.unwrap();
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_offerless_prepares_actual_offer_before_200_and_waits_for_ack_after_receipt() {
    let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
    let invite = io_invite("offerless-held-200");
    io_ring(&mut service, &mut wire, &invite).await;
    let mut answering = Box::pin(service.answer(g()));
    let response = tokio::select! {
        _ = &mut answering => panic!("answer completed without the 200 receipt"),
        message = wire.recv() => message.unwrap().response.unwrap(),
    };
    assert_eq!(response.status, 200);
    assert_eq!(
        *media.local_offers.lock().unwrap(),
        vec![response.data.body.clone()]
    );
    assert_eq!(
        *media.events.lock().unwrap(),
        vec![
            "response100",
            "response180",
            "prepare_local_offer",
            "response200"
        ]
    );
    assert!(media.prepared.lock().unwrap().is_empty());
    io_assert_no_transmit(&media);
    let receipt_at = Instant::now();
    receipts.recv().await.unwrap().send(Ok(())).unwrap();
    answering.await.unwrap();
    let pending = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap();
    assert!(pending.deadline >= receipt_at + Duration::from_secs(32));
    assert!(pending.next_response_retry >= receipt_at + Duration::from_millis(500));
    assert_eq!(service.call.as_ref().unwrap().local_sdp, response.data.body);
    assert!(service.call.as_ref().unwrap().remote.is_none());
    io_assert_no_transmit(&media);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_offerless_evs_and_wb_ack_select_actual_offer_and_commit_once() {
    for evs in [true, false] {
        let (mut service, media, mut wire, task) = fixture();
        let (invite, local_tag, response) =
            io_accept(&mut service, &mut wire, "offerless-codec").await;
        io_assert_no_transmit(&media);
        let ack = io_ack("offerless-codec", &local_tag, &response, evs);
        let frozen_offer = response.data.body.clone();
        let deadline = service
            .call
            .as_ref()
            .unwrap()
            .initial_answer
            .as_ref()
            .unwrap()
            .deadline;
        service
            .handle_event(&request_event(ack.clone()))
            .await
            .unwrap();
        assert_eq!(service.snapshot.state, "active");
        assert_eq!(service.snapshot.codec, if evs { "EVS" } else { "AMR-WB" });
        let actual = media.configured.lock().unwrap().clone();
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].remote_ip, "192.0.2.20".parse::<IpAddr>().unwrap());
        assert_eq!(actual[0].remote_port, 49200);
        assert_eq!(matches!(actual[0].codec, AudioCodec::Evs { .. }), evs);
        assert_eq!(
            service.call.as_ref().unwrap().remote.as_ref(),
            Some(&actual[0])
        );
        assert_eq!(*media.start_modes.lock().unwrap(), vec![false]);
        assert_eq!(service.call.as_ref().unwrap().local_sdp, frozen_offer);
        assert!(service.call.as_ref().unwrap().initial_answer.is_none());
        let started_at = service.snapshot.started_at.clone();
        for _ in 0..2 {
            service
                .handle_event(&request_event(ack.clone()))
                .await
                .unwrap();
            service
                .handle_event(&request_event(invite.clone()))
                .await
                .unwrap();
        }
        service
            .tick(g(), deadline + Duration::from_secs(1))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
        assert_eq!(media.configured.lock().unwrap().len(), 1);
        assert_eq!(*media.start_modes.lock().unwrap(), vec![false]);
        assert_eq!(media.local_offers.lock().unwrap().len(), 1);
        assert_eq!(service.snapshot.started_at, started_at);
        assert_eq!(service.snapshot.state, "active");
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_offerless_retransmits_frozen_complete_offer_without_preparing_or_starting_again() {
    let (mut service, media, mut wire, task) = fixture();
    let (invite, _, response) = io_accept(&mut service, &mut wire, "offerless-retry").await;
    let original = initial_response_bytes(&response);
    let schedule = initial_pending_schedule(service.call.as_ref().unwrap()).unwrap();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(
        initial_pending_schedule(service.call.as_ref().unwrap()),
        Some(schedule)
    );
    for _ in 0..3 {
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
            initial_response_bytes(&io_recv!(wire).response.unwrap()),
            original
        );
        assert_eq!(media.local_offers.lock().unwrap().len(), 1);
        assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
        io_assert_no_transmit(&media);
    }
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_offerless_wrong_connection_tag_or_cseq_ack_cannot_consume_local_offer() {
    let (mut service, media, mut wire, task) = fixture();
    let (_, local_tag, response) = io_accept(&mut service, &mut wire, "offerless-ack-id").await;
    let ack = io_ack("offerless-ack-id", &local_tag, &response, true);
    let mut wrong_tag = ack.clone();
    wrong_tag
        .data
        .headers
        .set(Header::new("To", "<sip:a@ims.test>;tag=other").unwrap());
    let mut wrong_cseq = ack.clone();
    wrong_cseq
        .data
        .headers
        .set(Header::new("CSeq", "15 ACK").unwrap());
    let schedule = initial_pending_schedule(service.call.as_ref().unwrap());
    for event in [
        request_event(wrong_tag),
        request_event(wrong_cseq),
        initial_request_on(ack.clone(), SipConnectionId::Incoming(99)),
    ] {
        service.handle_event(&event).await.unwrap();
        assert_eq!(
            initial_pending_schedule(service.call.as_ref().unwrap()),
            schedule
        );
        assert_eq!(service.snapshot.state, "connecting");
        assert!(service.call.as_ref().unwrap().remote.is_none());
        io_assert_no_transmit(&media);
        assert!(wire.try_recv().is_err());
    }
    service.handle_event(&request_event(ack)).await.unwrap();
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![false]);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_offerless_invalid_ack_answer_ends_dialog_once_without_enabling_transmit() {
    for invalid in [
        "missing",
        "not-sdp",
        "content-type",
        "payload",
        "mapping",
        "evs-dtx",
        "evs-cmr",
        "wb-framing",
        "wb-mode",
        "direction",
    ] {
        let (mut service, media, mut wire, task) = fixture();
        let (_, local_tag, response) =
            io_accept(&mut service, &mut wire, "offerless-invalid").await;
        let mut ack = io_ack(
            "offerless-invalid",
            &local_tag,
            &response,
            !invalid.starts_with("wb-"),
        );
        let valid = String::from_utf8(ack.data.body.clone()).unwrap();
        match invalid {
            "missing" => ack.data.body.clear(),
            "not-sdp" => ack.data.body = b"invalid session description".to_vec(),
            "content-type" => {
                ack.data
                    .headers
                    .set(Header::new("Content-Type", "text/plain").unwrap());
            }
            "payload" => {
                let negotiated = sdp::negotiate_audio_offer_with_modes(&valid, true, &[]).unwrap();
                // 127 is absent from the real local offer, irrespective of its chosen PT.
                ack.data.body = valid
                    .replace(&negotiated.payload_type.to_string(), "127")
                    .into_bytes();
            }
            "mapping" => ack.data.body = valid.replace("EVS/16000", "AMR-WB/16000").into_bytes(),
            "evs-dtx" => ack.data.body = valid.replace("dtx=0", "dtx=1").into_bytes(),
            "evs-cmr" => ack.data.body = valid.replace("cmr=-1", "cmr=0").into_bytes(),
            "wb-framing" => {
                assert!(valid.contains("octet-align=1"));
                ack.data.body = valid.replace("octet-align=1", "octet-align=0").into_bytes();
            }
            "wb-mode" => {
                assert!(valid.contains("mode-set="));
                ack.data.body = valid.replace("mode-set=", "mode-set=9,").into_bytes();
            }
            "direction" => ack.data.body = valid.replace("a=sendrecv", "a=recvonly").into_bytes(),
            _ => unreachable!(),
        }
        assert!(
            service
                .handle_event(&request_event(ack.clone()))
                .await
                .is_err()
        );
        assert_eq!(io_recv!(wire).request.unwrap().method, Method::BYE);
        assert_eq!(service.snapshot.state, "failed", "case {invalid}");
        assert!(!service.snapshot.media_ready);
        assert!(service.call.as_ref().unwrap().remote.is_none());
        assert!(service.call.as_ref().unwrap().initial_answer.is_none());
        io_assert_no_transmit(&media);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        for _ in 0..2 {
            service
                .handle_event(&request_event(ack.clone()))
                .await
                .unwrap();
            service
                .tick(g(), Instant::now() + Duration::from_secs(60))
                .await
                .unwrap();
        }
        assert!(wire.try_recv().is_err(), "case {invalid}");
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
        io_assert_no_transmit(&media);
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_offerless_actual_offer_payload_history_survives_ack_selection() {
    let (mut service, media, mut wire, task) = fixture();
    let (_, local_tag, response) = io_accept(&mut service, &mut wire, "offerless-history").await;
    let offered = std::str::from_utf8(&response.data.body).unwrap();
    let evs = sdp::negotiate_audio_offer_with_modes(offered, true, &[]).unwrap();
    // Select WB in the ACK, then try to repurpose the unselected, advertised EVS PT.
    let ack = io_ack("offerless-history", &local_tag, &response, false);
    service.handle_event(&request_event(ack)).await.unwrap();
    let configured = media.configured.lock().unwrap().clone();
    let rebound = io_sdp_request(
        "offerless-history",
        &local_tag,
        15,
        Method::INVITE,
        p5_wb_offer(evs.payload_type, 49202, "octet-align=1;mode-set=2,5"),
    );
    service.handle_event(&request_event(rebound)).await.unwrap();
    assert_eq!(io_recv!(wire).response.unwrap().status, 488);
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![false]);
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.snapshot.codec, "AMR-WB");
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_offerless_prepare_failure_replays_488_and_never_publishes_a_200() {
    let (mut service, media, mut wire, task) = fixture();
    let invite = io_invite("offerless-prepare-failure");
    io_ring(&mut service, &mut wire, &invite).await;
    media.prepare_local_offer_fail.store(true, Ordering::SeqCst);
    assert_eq!(
        service.answer(g()).await.err().unwrap().code,
        "ims_media_prepare_failed"
    );
    let rejected = io_recv!(wire).response.unwrap();
    assert_eq!(rejected.status, 488);
    assert!(
        !media
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "response200")
    );
    io_assert_no_transmit(&media);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(service.snapshot.state, "failed");
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(
        initial_response_bytes(&io_recv!(wire).response.unwrap()),
        initial_response_bytes(&rejected)
    );
    assert_eq!(media.allocated.load(Ordering::SeqCst), 1);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_offerless_configure_or_start_failure_on_valid_ack_sends_one_bye_and_drains() {
    for fail_start in [false, true] {
        let (mut service, media, mut wire, task) = fixture();
        let (_, local_tag, response) =
            io_accept(&mut service, &mut wire, "offerless-commit-failure").await;
        let ack = io_ack("offerless-commit-failure", &local_tag, &response, true);
        if fail_start {
            media.start_fail.store(true, Ordering::SeqCst);
        } else {
            media.configure_fail.store(true, Ordering::SeqCst);
        }
        let error = service
            .handle_event(&request_event(ack.clone()))
            .await
            .err()
            .unwrap();
        assert_eq!(
            error.code,
            if fail_start {
                "ims_media_start_failed"
            } else {
                "ims_media_commit_failed"
            }
        );
        assert_eq!(io_recv!(wire).request.unwrap().method, Method::BYE);
        assert_eq!(service.snapshot.state, "failed");
        assert!(!service.snapshot.media_ready);
        assert!(service.call.as_ref().unwrap().initial_answer.is_none());
        let configured = media.configured.lock().unwrap().len();
        let started = media.started.load(Ordering::SeqCst);
        assert_eq!(configured, usize::from(fail_start));
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        service.handle_event(&request_event(ack)).await.unwrap();
        service
            .tick(g(), Instant::now() + Duration::from_secs(60))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
        assert_eq!(media.configured.lock().unwrap().len(), configured);
        assert_eq!(media.started.load(Ordering::SeqCst), started);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_offerless_ack_timeout_keeps_stop_failure_fence_until_explicit_cleanup() {
    let (mut service, media, mut wire, task) = fixture();
    let (_, local_tag, response) =
        io_accept(&mut service, &mut wire, "offerless-stop-failure").await;
    let ack = io_ack("offerless-stop-failure", &local_tag, &response, true);
    let deadline = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap()
        .deadline;
    media.stop_fail.store(true, Ordering::SeqCst);
    let error = service.tick(g(), deadline).await.err().unwrap();
    assert_eq!(error.code, "ims_media_stop_failed");
    assert!(error.uncertain);
    assert_eq!(io_recv!(wire).request.unwrap().method, Method::BYE);
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert!(service.call.as_ref().unwrap().uncertain);
    assert!(service.call.as_ref().unwrap().media_allocated);
    assert!(service.call.as_ref().unwrap().initial_answer.is_none());
    service.handle_event(&request_event(ack)).await.unwrap();
    service
        .tick(g(), deadline + Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    io_assert_no_transmit(&media);
    assert!(wire.try_recv().is_err());
    assert!(service.dial(g(), "+447700900123").await.is_err());
    media.stop_fail.store(false, Ordering::SeqCst);
    service.shutdown(g()).await.unwrap();
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 2);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    assert!(service.snapshot.uncertain);
    assert_eq!(service.next_deadline(), None);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_offerless_32_second_timeout_cannot_be_revived_by_old_ack_after_a_new_call() {
    let (mut service, media, mut wire, task) = fixture();
    let (_, local_tag, response) = io_accept(&mut service, &mut wire, "offerless-timeout").await;
    let ack = io_ack("offerless-timeout", &local_tag, &response, true);
    let deadline = service
        .call
        .as_ref()
        .unwrap()
        .initial_answer
        .as_ref()
        .unwrap()
        .deadline;
    service.tick(g(), deadline).await.unwrap();
    let bye = io_recv!(wire).request.unwrap();
    assert_eq!(bye.method, Method::BYE);
    assert_eq!(
        single(&bye.data.headers, "Call-ID").unwrap(),
        "offerless-timeout"
    );
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_initial_ack_timeout");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    io_assert_no_transmit(&media);
    service.dial(g(), "+447700900123").await.unwrap();
    let new_invite = io_recv!(wire).request.unwrap();
    assert_eq!(new_invite.method, Method::INVITE);
    let current_id = service.snapshot.call_id.clone();
    let configures = media.configured.lock().unwrap().len();
    service.handle_event(&request_event(ack)).await.unwrap();
    assert_eq!(service.snapshot.call_id, current_id);
    assert_eq!(service.snapshot.state, "outgoing");
    assert_eq!(media.configured.lock().unwrap().len(), configures);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_offerless_ack_after_deadline_before_tick_cleans_up_without_committing_media() {
    let (mut service, media, mut wire, task) = fixture();
    let (_, local_tag, response) = io_accept(&mut service, &mut wire, "offerless-late-ack").await;
    let ack = io_ack("offerless-late-ack", &local_tag, &response, true);
    service
        .call
        .as_mut()
        .unwrap()
        .initial_answer
        .as_mut()
        .unwrap()
        .deadline = Instant::now() - Duration::from_millis(1);
    service
        .handle_event(&request_event(ack.clone()))
        .await
        .unwrap();
    assert_eq!(io_recv!(wire).request.unwrap().method, Method::BYE);
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_initial_ack_timeout");
    assert!(service.call.as_ref().unwrap().remote.is_none());
    io_assert_no_transmit(&media);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service.handle_event(&request_event(ack)).await.unwrap();
    service
        .tick(g(), Instant::now() + Duration::from_secs(60))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_offerless_hangup_before_ack_defers_one_bye_and_never_commits_ack_media() {
    for acknowledge in [true, false] {
        let (mut service, media, mut wire, task) = fixture();
        let (_, local_tag, response) = io_accept(&mut service, &mut wire, "offerless-hangup").await;
        let ack = io_ack("offerless-hangup", &local_tag, &response, true);
        let deadline = service
            .call
            .as_ref()
            .unwrap()
            .initial_answer
            .as_ref()
            .unwrap()
            .deadline;
        service.hangup(g()).await.unwrap();
        service.hangup(g()).await.unwrap();
        assert_eq!(service.snapshot.state, "cancelling");
        assert!(media.transmit_held.load(Ordering::SeqCst));
        assert!(wire.try_recv().is_err());
        io_assert_no_transmit(&media);
        if acknowledge {
            service
                .handle_event(&request_event(ack.clone()))
                .await
                .unwrap();
        } else {
            service.tick(g(), deadline).await.unwrap();
        }
        assert_eq!(io_recv!(wire).request.unwrap().method, Method::BYE);
        assert_eq!(service.snapshot.state, "ended");
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        service.handle_event(&request_event(ack)).await.unwrap();
        service
            .tick(g(), deadline + Duration::from_secs(1))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
        io_assert_no_transmit(&media);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_offerless_cancel_before_answer_rejects_but_late_cancel_preserves_accepted_offer() {
    for accepted in [false, true] {
        let (mut service, media, mut wire, task) = fixture();
        let invite = io_invite("offerless-cancel");
        let local_tag = io_ring(&mut service, &mut wire, &invite).await;
        let response = if accepted {
            service.answer(g()).await.unwrap();
            Some(io_recv!(wire).response.unwrap())
        } else {
            None
        };
        service
            .handle_event(&request_event(ar_cancel(&invite)))
            .await
            .unwrap();
        assert_eq!(io_recv!(wire).response.unwrap().status, 200);
        if let Some(response) = response {
            assert!(wire.try_recv().is_err());
            assert_eq!(service.snapshot.state, "connecting");
            service
                .handle_event(&request_event(io_ack(
                    "offerless-cancel",
                    &local_tag,
                    &response,
                    true,
                )))
                .await
                .unwrap();
            assert_eq!(service.snapshot.state, "active");
            assert_eq!(*media.start_modes.lock().unwrap(), vec![false]);
        } else {
            assert_eq!(io_recv!(wire).response.unwrap().status, 487);
            assert_eq!(service.snapshot.state, "ended");
            assert_eq!(media.allocated.load(Ordering::SeqCst), 0);
            io_assert_no_transmit(&media);
        }
        assert!(wire.try_recv().is_err());
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_offerless_remote_bye_before_ack_cannot_be_undone_by_the_answer() {
    let (mut service, media, mut wire, task) = fixture();
    let (_, local_tag, response) = io_accept(&mut service, &mut wire, "offerless-remote-bye").await;
    let ack = io_ack("offerless-remote-bye", &local_tag, &response, true);
    service
        .handle_event(&request_event(inbound(
            "offerless-remote-bye",
            15,
            Method::BYE,
            Some(&local_tag),
        )))
        .await
        .unwrap();
    assert_eq!(io_recv!(wire).response.unwrap().status, 200);
    assert_eq!(service.snapshot.state, "ended");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service.handle_event(&request_event(ack)).await.unwrap();
    service
        .tick(g(), Instant::now() + Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "ended");
    io_assert_no_transmit(&media);
    assert!(wire.try_recv().is_err());
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_offerless_unknown_200_write_fences_rx_and_only_strict_ack_sends_cleanup_bye() {
    let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
    let invite = io_invite("offerless-unknown");
    let local_tag = io_ring(&mut service, &mut wire, &invite).await;
    let mut answering = Box::pin(service.answer(g()));
    let response = tokio::select! {
        _ = &mut answering => panic!("answer completed without receipt"),
        message = wire.recv() => message.unwrap().response.unwrap(),
    };
    assert_eq!(response.status, 200);
    io_assert_no_transmit(&media);
    receipts
        .recv()
        .await
        .unwrap()
        .send(Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "offerless unknown 200",
        )))
        .unwrap();
    assert!(answering.await.err().unwrap().uncertain);
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert!(!service.call.as_ref().unwrap().initial_reply_confirmed);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    service.handle_event(&request_event(invite)).await.unwrap();
    let ack = io_ack("offerless-unknown", &local_tag, &response, true);
    service
        .handle_event(&initial_request_on(
            ack.clone(),
            SipConnectionId::Incoming(99),
        ))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    service
        .handle_event(&request_event(ack.clone()))
        .await
        .unwrap();
    assert_eq!(io_recv!(wire).request.unwrap().method, Method::BYE);
    service.handle_event(&request_event(ack)).await.unwrap();
    service
        .tick(g(), Instant::now() + Duration::from_secs(60))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    io_assert_no_transmit(&media);
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn unassociated_prack_never_accepts_sdp_or_mutates_early_or_active_dialog() {
    for active in [false, true] {
        let (mut service, media, mut wire, task) = fixture();
        let invite = io_invite("unassociated-prack");
        let local_tag = io_ring(&mut service, &mut wire, &invite).await;
        if active {
            service.answer(g()).await.unwrap();
            let response = io_recv!(wire).response.unwrap();
            service
                .handle_event(&request_event(io_ack(
                    "unassociated-prack",
                    &local_tag,
                    &response,
                    true,
                )))
                .await
                .unwrap();
        }
        let state = service.snapshot.state.clone();
        let remote = service.call.as_ref().unwrap().remote.clone();
        let target = service
            .call
            .as_ref()
            .unwrap()
            .dialog
            .as_ref()
            .unwrap()
            .remote_target
            .to_string();
        let timer = service
            .call
            .as_ref()
            .unwrap()
            .session_timer
            .as_ref()
            .map(|timer| timer.expires_at);
        let prepared = media.prepared.lock().unwrap().len();
        let configured = media.configured.lock().unwrap().clone();
        let starts = media.start_modes.lock().unwrap().clone();
        for (index, rack) in [
            None,
            Some("invalid"),
            Some("1 14 INVITE"),
            Some("1 14 invite"),
            Some("1 99 INVITE"),
        ]
        .into_iter()
        .enumerate()
        {
            for with_sdp in [false, true] {
                let mut prack = inbound(
                    "unassociated-prack",
                    15 + (index as u32) * 2 + u32::from(with_sdp),
                    Method::PRACK,
                    Some(&local_tag),
                );
                if let Some(rack) = rack {
                    push(&mut prack.data.headers, "RAck", rack).unwrap();
                }
                prack
                    .data
                    .headers
                    .set(Header::new("Contact", "<sip:b@must-not-adopt.ims.test>").unwrap());
                push(
                    &mut prack.data.headers,
                    "Session-Expires",
                    "90;refresher=uac",
                )
                .unwrap();
                if with_sdp {
                    prack.data.body = p5_wb_offer(111, 49204, "octet-align=1;mode-set=2,5");
                    push(&mut prack.data.headers, "Content-Type", "application/sdp").unwrap();
                }
                service.handle_event(&request_event(prack)).await.unwrap();
                let status = match rack {
                    Some("1 14 INVITE" | "1 99 INVITE") => 481,
                    _ => 400,
                };
                assert_eq!(io_recv!(wire).response.unwrap().status, status);
                assert_eq!(service.snapshot.state, state);
                assert_eq!(service.call.as_ref().unwrap().remote, remote);
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
                    target
                );
                assert_eq!(
                    service
                        .call
                        .as_ref()
                        .unwrap()
                        .session_timer
                        .as_ref()
                        .map(|timer| timer.expires_at),
                    timer
                );
                assert_eq!(media.prepared.lock().unwrap().len(), prepared);
                assert_eq!(*media.configured.lock().unwrap(), configured);
                assert_eq!(*media.start_modes.lock().unwrap(), starts);
                assert!(wire.try_recv().is_err());
            }
        }
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn initial_offerless_pending_offer_rejects_new_offers_but_ack_preserves_update_target_and_timer()
 {
    let (mut service, media, mut wire, task) = fixture();
    let call_id = "offerless-pending-glare";
    let mut invite = io_invite(call_id);
    push(
        &mut invite.data.headers,
        "Session-Expires",
        "180;refresher=uac",
    )
    .unwrap();
    let local_tag = io_ring(&mut service, &mut wire, &invite).await;
    service.answer(g()).await.unwrap();
    let response = io_recv!(wire).response.unwrap();
    assert_eq!(response.status, 200);
    let schedule = initial_pending_schedule(service.call.as_ref().unwrap());
    let initial_timer = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap();
    let initial_expiry = initial_timer.expires_at;
    assert_eq!(initial_timer.interval, 180);
    for (seq, method) in [(15, Method::UPDATE), (16, Method::INVITE)] {
        let mut competing = io_sdp_request(
            call_id,
            &local_tag,
            seq,
            method,
            p5_wb_offer(111, 49204, "octet-align=1;mode-set=2,5"),
        );
        competing
            .data
            .headers
            .set(Header::new("Contact", "<sip:b@must-not-adopt.ims.test>").unwrap());
        push(
            &mut competing.data.headers,
            "Session-Expires",
            "600;refresher=uas",
        )
        .unwrap();
        service
            .handle_event(&request_event(competing))
            .await
            .unwrap();
        assert_eq!(io_recv!(wire).response.unwrap().status, 491);
        assert_eq!(service.snapshot.state, "connecting");
        let call = service.call.as_ref().unwrap();
        assert!(call.remote.is_none());
        assert!(call.pending_reinvite.is_none());
        assert!(call.accepted_reinvites.is_empty());
        assert_eq!(call.local_sdp, response.data.body);
        assert_eq!(initial_pending_schedule(call), schedule);
        assert_eq!(
            call.session_timer.as_ref().unwrap().expires_at,
            initial_expiry
        );
        assert_eq!(
            call.dialog.as_ref().unwrap().remote_target.to_string(),
            "sip:b@peer.ims.test"
        );
        assert!(media.prepared.lock().unwrap().is_empty());
        io_assert_no_transmit(&media);
    }

    let mut update = inbound(call_id, 17, Method::UPDATE, Some(&local_tag));
    update
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@latest-target.ims.test>").unwrap());
    push(
        &mut update.data.headers,
        "Session-Expires",
        "300;refresher=uas",
    )
    .unwrap();
    service.handle_event(&request_event(update)).await.unwrap();
    let updated = io_recv!(wire).response.unwrap();
    assert_eq!(updated.status, 200);
    assert!(updated.data.body.is_empty());
    let call = service.call.as_ref().unwrap();
    assert_eq!(initial_pending_schedule(call), schedule);
    let timer = call.session_timer.as_ref().unwrap();
    assert_eq!(timer.interval, 300);
    assert!(timer.local_refresher);
    let timer_after_update = (timer.expires_at, timer.termination_at, timer.refresh_at);
    assert!(timer.expires_at > initial_expiry);
    io_assert_no_transmit(&media);

    service
        .handle_event(&request_event(io_ack(call_id, &local_tag, &response, true)))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "active");
    let call = service.call.as_ref().unwrap();
    assert_eq!(
        call.dialog.as_ref().unwrap().remote_target.to_string(),
        "sip:b@latest-target.ims.test"
    );
    assert_eq!(call.local_sdp, response.data.body);
    let timer = call.session_timer.as_ref().unwrap();
    assert_eq!(timer.interval, 300);
    assert!(timer.local_refresher);
    assert_eq!(
        (timer.expires_at, timer.termination_at, timer.refresh_at),
        timer_after_update
    );
    assert_eq!(media.configured.lock().unwrap().len(), 1);
    assert_eq!(*media.start_modes.lock().unwrap(), vec![false]);
    assert!(wire.try_recv().is_err());
    service.hangup(g()).await.unwrap();
    let bye = io_recv!(wire).request.unwrap();
    assert_eq!(bye.method, Method::BYE);
    assert_eq!(bye.uri.to_string(), "sip:b@latest-target.ims.test");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn initial_offerless_unknown_200_and_cleanup_bye_failure_retry_stop_once_without_duplicate_work()
 {
    for acknowledge in [true, false] {
        let (mut service, media, mut wire, task) = ar_failed_bye_writer_fixture(true);
        let call_id = "offerless-unknown-cleanup-failure";
        let invite = io_invite(call_id);
        let local_tag = io_ring(&mut service, &mut wire, &invite).await;
        media.stop_fail.store(true, Ordering::SeqCst);
        let initial_error = service.answer(g()).await.err().unwrap();
        assert!(initial_error.uncertain);
        let response = io_recv!(wire).response.unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(
            *media.local_offers.lock().unwrap(),
            vec![response.data.body.clone()]
        );
        let ack = io_ack(call_id, &local_tag, &response, true);
        let call = service.call.as_ref().unwrap();
        let pending = call.initial_answer.as_ref().unwrap();
        let deadline = pending.deadline;
        assert_eq!(pending.next_response_retry, deadline);
        assert!(pending.hangup_requested);
        assert!(!call.initial_reply_confirmed);
        assert!(call.media_allocated);
        assert!(call.uncertain);
        assert_eq!(service.snapshot.state, "failed");
        assert!(service.snapshot.uncertain);
        assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
        io_assert_no_transmit(&media);

        // The first failed stop retains ownership. The ACK/timeout cleanup is
        // one explicit retry even though the cleanup BYE also has no receipt.
        media.stop_fail.store(false, Ordering::SeqCst);
        let cleanup_error = if acknowledge {
            service
                .handle_event(&request_event(ack.clone()))
                .await
                .err()
                .unwrap()
        } else {
            service.tick(g(), deadline).await.err().unwrap()
        };
        assert!(cleanup_error.uncertain);
        let bye = io_recv!(wire).request.unwrap();
        assert_eq!(bye.method, Method::BYE);
        assert_eq!(single(&bye.data.headers, "Call-ID").unwrap(), call_id);
        let call = service.call.as_ref().unwrap();
        assert!(call.dialog_terminated);
        assert!(call.initial_answer.is_none());
        assert!(!call.media_allocated);
        assert!(call.uncertain);
        assert_eq!(service.snapshot.state, "failed");
        assert!(service.snapshot.uncertain);
        assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        let events = media.events.lock().unwrap().clone();

        for _ in 0..3 {
            service
                .handle_event(&request_event(ack.clone()))
                .await
                .unwrap();
            service
                .handle_event(&request_event(invite.clone()))
                .await
                .unwrap();
            service
                .tick(g(), deadline + Duration::from_secs(1))
                .await
                .unwrap();
        }
        assert!(wire.try_recv().is_err());
        assert_eq!(*media.events.lock().unwrap(), events);
        assert_eq!(media.local_offers.lock().unwrap().len(), 1);
        assert!(media.prepared.lock().unwrap().is_empty());
        io_assert_no_transmit(&media);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert_eq!(service.snapshot.state, "failed");
        assert!(service.snapshot.uncertain);
        assert!(service.dial(g(), "+447700900123").await.is_err());
        assert!(wire.try_recv().is_err());
        drop(service);
        task.await.unwrap();
    }
}
