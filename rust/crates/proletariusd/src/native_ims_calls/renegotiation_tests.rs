// Protocol-owner coverage: the media transition and the retained SIP commit
// have different boundaries. These tests deliberately observe both.
fn p5_wb_offer(payload: u8, port: u16, parameters: &str) -> Vec<u8> {
    format!(
        "v=0\r\no=- 99 2 IN IP4 192.0.2.20\r\ns=peer\r\nc=IN IP4 192.0.2.20\r\nt=0 0\r\nm=audio {port} RTP/AVP {payload}\r\na=rtpmap:{payload} AMR-WB/16000\r\na=fmtp:{payload} {parameters}\r\na=ptime:20\r\na=maxptime:20\r\na=sendrecv\r\n"
    )
    .into_bytes()
}

fn p5_evs_offer(payload: u8, port: u16) -> Vec<u8> {
    format!(
        "v=0\r\no=- 99 3 IN IP4 192.0.2.20\r\ns=peer\r\nc=IN IP4 192.0.2.20\r\nt=0 0\r\nm=audio {port} RTP/AVP {payload}\r\na=rtpmap:{payload} EVS/16000\r\na=fmtp:{payload} br=13.2;bw=wb;evs-mode-switch=0;dtx=0;cmr=-1;ch-aw-recv=-1\r\na=ptime:20\r\na=maxptime:20\r\na=sendrecv\r\n"
    )
    .into_bytes()
}

fn p5_offer_request(
    call_id: &str,
    local_tag: &str,
    seq: u32,
    method: Method,
    body: Vec<u8>,
) -> Request {
    let mut request = in_dialog(call_id, seq, method, local_tag);
    request.data.body = body;
    push(&mut request.data.headers, "Content-Type", "application/sdp").unwrap();
    request
}

async fn p5_establish(
    service: &mut Service,
    wire: &mut mpsc::UnboundedReceiver<Wire>,
) -> (String, String) {
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
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.snapshot.codec, "EVS");
    (
        single(&initial.data.headers, "Call-ID").unwrap().to_owned(),
        tag(single(&initial.data.headers, "From").unwrap())
            .unwrap()
            .to_owned(),
    )
}

#[tokio::test]
async fn p5_offered_reinvite_switches_media_before_ack_and_commits_once() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let retained = service.call.as_ref().unwrap().remote.clone();
    let initial_configures = media.configured.lock().unwrap().len();
    media.events.lock().unwrap().clear();
    let mut invite = p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::INVITE,
        p5_wb_offer(
            111,
            49200,
            "octet-align=1;mode-set=2,5;mode-change-period=2;mode-change-neighbor=1",
        ),
    );
    invite
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@new-contact.ims.test>").unwrap());
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    let accepted = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(accepted.status, 200);
    assert_eq!(
        *media.events.lock().unwrap(),
        vec!["prepare", "response200", "configure"]
    );
    assert_eq!(service.snapshot.codec, "AMR-WB");
    assert_eq!(service.call.as_ref().unwrap().remote, retained);
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 1);
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
        "sip:b@new-contact.ims.test"
    );
    assert_eq!(
        media.configured.lock().unwrap().len(),
        initial_configures + 1
    );
    assert_eq!(
        media.configured.lock().unwrap().last().unwrap().remote_port,
        49200
    );
    let prepares = media.prepared.lock().unwrap().len();
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

    // Accepted absorbs duplicates; only the TU timer resends the exact 200.
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    assert!(wire.try_recv().is_err());
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
        initial_configures + 1
    );
    service.tick(g(), retry_at).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap(), accepted);
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
    assert_eq!(media.prepared.lock().unwrap().len(), prepares);
    assert_eq!(
        media.configured.lock().unwrap().len(),
        initial_configures + 1
    );

    let wrong_seq = in_dialog(&call_id, 99, Method::ACK, &local_tag);
    service
        .handle_event(&request_event(wrong_seq))
        .await
        .unwrap();
    let mut wrong_connection = request_event(in_dialog(&call_id, 2, Method::ACK, &local_tag));
    if let RegisteredSipEvent::Message { connection, .. } = &mut wrong_connection.event {
        *connection = SipConnectionId::Incoming(99);
    }
    service.handle_event(&wrong_connection).await.unwrap();
    let mut wrong_method = in_dialog(&call_id, 2, Method::ACK, &local_tag);
    wrong_method
        .data
        .headers
        .set(Header::new("CSeq", "2 INVITE").unwrap());
    service
        .handle_event(&request_event(wrong_method))
        .await
        .unwrap();
    let mut wrong_dialog = in_dialog(&call_id, 2, Method::ACK, &local_tag);
    wrong_dialog
        .data
        .headers
        .set(Header::new("From", "<sip:b@ims.test>;tag=unrelated-dialog").unwrap());
    service
        .handle_event(&request_event(wrong_dialog))
        .await
        .unwrap();
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_some());
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 1);
    assert_eq!(
        media.configured.lock().unwrap().len(),
        initial_configures + 1
    );

    let ack = in_dialog(&call_id, 2, Method::ACK, &local_tag);
    service
        .handle_event(&request_event(ack.clone()))
        .await
        .unwrap();
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 2);
    assert_eq!(
        service
            .call
            .as_ref()
            .unwrap()
            .remote
            .as_ref()
            .unwrap()
            .remote_port,
        49200
    );
    assert_eq!(
        media.configured.lock().unwrap().len(),
        initial_configures + 1
    );
    service.handle_event(&request_event(ack)).await.unwrap();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert!(wire.try_recv().is_err());
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
        initial_configures + 1
    );
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_update_supports_codec_roundtrip_and_same_payload_policy_changes() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let updates = [
        p5_wb_offer(
            111,
            49200,
            "octet-align=1;mode-set=2,5;mode-change-period=2;mode-change-neighbor=1",
        ),
        p5_wb_offer(
            111,
            49202,
            "octet-align=1;mode-set=0,2,5,8;mode-change-period=1;mode-change-neighbor=0",
        ),
        p5_evs_offer(96, 49204),
    ];
    let initial = media.configured.lock().unwrap().len();
    for (index, body) in updates.into_iter().enumerate() {
        media.events.lock().unwrap().clear();
        let update = p5_offer_request(&call_id, &local_tag, index as u32 + 2, Method::UPDATE, body);
        service.handle_event(&request_event(update)).await.unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
        assert_eq!(
            *media.events.lock().unwrap(),
            vec!["prepare", "response200", "configure"]
        );
        assert_eq!(
            service.call.as_ref().unwrap().local_sdp_version,
            index as u64 + 2
        );
        assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
        assert_eq!(media.configured.lock().unwrap().len(), initial + index + 1);
        assert_eq!(
            service.snapshot.codec,
            if index == 2 { "EVS" } else { "AMR-WB" }
        );
        if index == 1 {
            let configured = media.configured.lock().unwrap();
            let AudioCodec::AmrWb { mode, policy, .. } = configured.last().unwrap().codec else {
                panic!("policy update must retain AMR-WB");
            };
            assert_eq!(mode, 8);
            assert_eq!(policy.modes().collect::<Vec<_>>(), vec![0, 2, 5, 8]);
            assert_eq!(policy.next_mode(2, Some(8), 1), 8);
        }
    }
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_prepare_failure_is_cached_rejection_and_keeps_previous_call_live() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let original = service.call.as_ref().unwrap().remote.clone();
    let original_media = media.configured.lock().unwrap().clone();
    media.prepare_fail.store(true, Ordering::SeqCst);
    let invite = p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::INVITE,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    );
    service
        .handle_event(&request_event(invite.clone()))
        .await
        .unwrap();
    let rejection = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(rejection.status, 488);
    let attempts = media.prepared.lock().unwrap().len();
    let prepare_events = media
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.as_str() == "prepare")
        .count();
    let discards = media.discards.load(Ordering::SeqCst);
    service.handle_event(&request_event(invite)).await.unwrap();
    let duplicate = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(duplicate.status, 488);
    assert_eq!(duplicate.data.body, rejection.data.body);
    assert_eq!(media.prepared.lock().unwrap().len(), attempts);
    assert_eq!(
        media
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.as_str() == "prepare")
            .count(),
        prepare_events
    );
    assert_eq!(media.discards.load(Ordering::SeqCst), discards);
    assert_eq!(discards, 1);
    assert_eq!(*media.configured.lock().unwrap(), original_media);
    assert_eq!(service.call.as_ref().unwrap().remote, original);
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 1);
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
    media.prepare_fail.store(false, Ordering::SeqCst);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_post_answer_commit_failure_ends_dialog_and_drains_media() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    media.configure_fail.store(true, Ordering::SeqCst);
    let invite = p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::INVITE,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    );
    let _ = service.handle_event(&request_event(invite)).await;
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_media_commit_failed");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_pending_codec_switch_retries_200_and_timeout_bye_uses_new_contact() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let mut invite = p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::INVITE,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    );
    invite
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@new-contact.ims.test>").unwrap());
    service.handle_event(&request_event(invite)).await.unwrap();
    let accepted = wire.recv().await.unwrap().response.unwrap();
    let prepared = media.prepared.lock().unwrap().len();
    let configured = media.configured.lock().unwrap().len();
    let transaction = service
        .call
        .as_ref()
        .unwrap()
        .accepted_reinvites
        .back()
        .unwrap();
    let retry_at = transaction.next_response_retry.unwrap();
    let deadline = transaction.deadline;
    service.tick(g(), retry_at).await.unwrap();
    let retry = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(retry, accepted);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    service.tick(g(), deadline).await.unwrap();
    let bye = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(bye.method, Method::BYE);
    assert_eq!(bye.uri.to_string(), "sip:b@new-contact.ims.test");
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_reinvite_ack_timeout");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_payload_history_rejects_old_codec_or_framing_reuse_after_roundtrip() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    for (seq, body) in [
        (2, p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5")),
        (3, p5_evs_offer(96, 49202)),
    ] {
        service
            .handle_event(&request_event(p5_offer_request(
                &call_id,
                &local_tag,
                seq,
                Method::UPDATE,
                body,
            )))
            .await
            .unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    }
    let previous = media.configured.lock().unwrap().clone();
    let prepares = media.prepared.lock().unwrap().len();
    for (seq, body) in [
        (4, p5_evs_offer(111, 49204)),
        (5, p5_wb_offer(111, 49206, "octet-align=0;mode-set=2,5")),
        // PT98 was advertised in the original offer although EVS was selected.
        (6, p5_evs_offer(98, 49208)),
    ] {
        service
            .handle_event(&request_event(p5_offer_request(
                &call_id,
                &local_tag,
                seq,
                Method::UPDATE,
                body,
            )))
            .await
            .unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 488);
    }
    assert_eq!(*media.configured.lock().unwrap(), previous);
    assert_eq!(media.prepared.lock().unwrap().len(), prepares);
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 3);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_offerless_ack_cannot_change_explicit_mode_set_or_framing() {
    for illegal_parameters in ["octet-align=1;mode-set=2", "octet-align=0;mode-set=2,5"] {
        let (mut service, media, mut wire, task) = fixture();
        let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
        service
            .handle_event(&request_event(p5_offer_request(
                &call_id,
                &local_tag,
                2,
                Method::UPDATE,
                p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
            )))
            .await
            .unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
        let prepared = media.prepared.lock().unwrap().len();
        let configured = media.configured.lock().unwrap().len();
        let mut invite = in_dialog(&call_id, 3, Method::INVITE, &local_tag);
        invite.data.body.clear();
        service.handle_event(&request_event(invite)).await.unwrap();
        let offered = wire.recv().await.unwrap().response.unwrap();
        assert_eq!(offered.status, 200);
        assert!(
            String::from_utf8(offered.data.body)
                .unwrap()
                .contains("mode-set=2,5")
        );
        assert_eq!(media.prepared.lock().unwrap().len(), prepared);
        let ack = p5_offer_request(
            &call_id,
            &local_tag,
            3,
            Method::ACK,
            p5_wb_offer(111, 49202, illegal_parameters),
        );
        service.handle_event(&request_event(ack)).await.unwrap();
        assert_eq!(
            wire.recv().await.unwrap().request.unwrap().method,
            Method::BYE
        );
        assert_eq!(service.snapshot.last_error, "ims_reinvite_ack_sdp_invalid");
        assert_eq!(service.snapshot.state, "failed");
        assert_eq!(media.prepared.lock().unwrap().len(), prepared);
        assert_eq!(media.configured.lock().unwrap().len(), configured);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn p5_offerless_ack_may_change_peer_transmit_policy_within_actual_offer() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    service
        .handle_event(&request_event(p5_offer_request(
            &call_id,
            &local_tag,
            2,
            Method::UPDATE,
            p5_wb_offer(
                111,
                49200,
                "octet-align=1;mode-set=2,5;mode-change-period=2;mode-change-neighbor=1",
            ),
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    let prepared = media.prepared.lock().unwrap().len();
    let configured = media.configured.lock().unwrap().len();
    let mut invite = in_dialog(&call_id, 3, Method::INVITE, &local_tag);
    invite.data.body.clear();
    service.handle_event(&request_event(invite)).await.unwrap();
    let offered = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(offered.status, 200);
    let offer_text = String::from_utf8(offered.data.body).unwrap();
    assert!(offer_text.contains("mode-set=2,5"));
    assert!(!offer_text.contains("mode-change-period=2"));
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    let ack = p5_offer_request(
        &call_id,
        &local_tag,
        3,
        Method::ACK,
        p5_wb_offer(
            111,
            49202,
            "octet-align=1;mode-set=2,5;mode-change-period=1;mode-change-neighbor=0",
        ),
    );
    service
        .handle_event(&request_event(ack.clone()))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 3);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared + 1);
    assert_eq!(media.configured.lock().unwrap().len(), configured + 1);
    let remote = service.call.as_ref().unwrap().remote.as_ref().unwrap();
    let AudioCodec::AmrWb { policy, .. } = remote.codec else {
        panic!("ACK retains offered AMR-WB")
    };
    assert_eq!(remote.remote_port, 49202);
    assert_eq!(policy.next_mode(2, Some(5), 1), 5);
    service.handle_event(&request_event(ack)).await.unwrap();
    assert_eq!(media.configured.lock().unwrap().len(), configured + 1);
    assert!(wire.try_recv().is_err());
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

// Every 200 writer receipt is controlled by the test; ordinary outgoing
// requests still complete normally. This catches configuring on queueing.
fn p5_controlled_writer_fixture() -> (
    Service,
    Arc<FakeMedia>,
    mpsc::UnboundedReceiver<Wire>,
    mpsc::UnboundedReceiver<oneshot::Sender<std::io::Result<()>>>,
    tokio::task::JoinHandle<()>,
) {
    let (commands, mut input) = mpsc::channel(32);
    let (sent, output) = mpsc::unbounded_channel();
    let (receipts, controlled) = mpsc::unbounded_channel();
    let media = Arc::new(FakeMedia::default());
    let writer_media = media.clone();
    let task = tokio::spawn(async move {
        while let Some(command) = input.recv().await {
            match command {
                Command::SendUntracked { request, reply, .. } => {
                    sent.send(Wire {
                        request: Some(request),
                        response: None,
                        connection: SipConnectionId::Primary,
                    })
                    .unwrap();
                    let _ = reply.send(Ok(()));
                }
                Command::RespondDialog {
                    connection,
                    response,
                    reply,
                    ..
                } => {
                    let controlled = response.status == 200;
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
                    if controlled {
                        receipts.send(reply).unwrap();
                    } else {
                        let _ = reply.send(Ok(()));
                    }
                }
                _ => panic!("voice must not use tracked SMS transactions"),
            }
        }
    });
    let service = Service::new(g(), config(), commands, media.clone()).unwrap();
    (service, media, output, controlled, task)
}

#[tokio::test]
async fn p5_codec_transmit_waits_for_successful_answer_writer_receipt() {
    let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let original_configures = media.configured.lock().unwrap().len();
    media.events.lock().unwrap().clear();
    let event = request_event(p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::INVITE,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    ));
    let mut handling = Box::pin(service.handle_event(&event));
    tokio::select! {
        result = &mut handling => panic!("answer finished before writer receipt: {result:?}"),
        observed = wire.recv() => assert_eq!(observed.unwrap().response.unwrap().status, 200),
    }
    assert_eq!(
        *media.events.lock().unwrap(),
        vec!["prepare", "response200"]
    );
    assert_eq!(media.configured.lock().unwrap().len(), original_configures);
    receipts.recv().await.unwrap().send(Ok(())).unwrap();
    handling.await.unwrap();
    assert_eq!(
        *media.events.lock().unwrap(),
        vec!["prepare", "response200", "configure"]
    );
    assert_eq!(
        media.configured.lock().unwrap().len(),
        original_configures + 1
    );
    assert_eq!(service.snapshot.codec, "AMR-WB");
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 1);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_unknown_answer_write_does_not_commit_media_and_drains_prepared_resources() {
    let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let original_media = media.configured.lock().unwrap().clone();
    let event = request_event(p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::INVITE,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    ));
    let mut handling = Box::pin(service.handle_event(&event));
    tokio::select! {
        result = &mut handling => panic!("answer finished before writer receipt: {result:?}"),
        observed = wire.recv() => assert_eq!(observed.unwrap().response.unwrap().status, 200),
    }
    receipts
        .recv()
        .await
        .unwrap()
        .send(Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "controlled answer write failure",
        )))
        .unwrap();
    assert!(handling.await.is_err());
    assert_eq!(*media.configured.lock().unwrap(), original_media);
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert_eq!(media.discards.load(Ordering::SeqCst), 1);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_refresh_answer_is_validated_against_actual_request_and_ack_uses_new_contact() {
    for answer_modes in ["2", "2,5"] {
        let (mut service, media, mut wire, task) = fixture();
        let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
        service
            .handle_event(&request_event(p5_offer_request(
                &call_id,
                &local_tag,
                2,
                Method::UPDATE,
                p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
            )))
            .await
            .unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
        let original_configures = media.configured.lock().unwrap().len();
        let now = Instant::now();
        let call = service.call.as_mut().unwrap();
        // Make the retained local offer stricter than the current remote
        // negotiation. Equality to current remote would accept the bad answer
        // and reject the good one, so this proves the actual offer is used.
        call.local_sdp = String::from_utf8(call.local_sdp.clone())
            .unwrap()
            .replace("mode-set=2,5", "mode-set=2")
            .into_bytes();
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
        let refresh = wire.recv().await.unwrap().request.unwrap();
        assert_eq!(refresh.method, Method::INVITE);
        let offered = String::from_utf8(refresh.data.body.clone()).unwrap();
        assert!(offered.contains("mode-set=2;"));
        assert!(!offered.contains("mode-set=2,5"));
        let mut response = response_event(
            &refresh,
            200,
            p5_wb_offer(
                111,
                49202,
                &format!("octet-align=1;mode-set={answer_modes}"),
            ),
            None,
        );
        let RegisteredSipEvent::Message { message, .. } = &mut response.event else {
            unreachable!()
        };
        let RegisteredSipMessage::Response { headers, .. } = message.as_mut() else {
            unreachable!()
        };
        headers.set(Header::new("Contact", "<sip:b@refresh-contact.ims.test>").unwrap());
        service.handle_event(&response).await.unwrap();
        let ack = wire.recv().await.unwrap().request.unwrap();
        assert_eq!(ack.method, Method::ACK);
        assert_eq!(ack.uri.to_string(), "sip:b@refresh-contact.ims.test");
        if answer_modes == "2" {
            assert_eq!(service.snapshot.state, "active");
            assert_eq!(
                media.configured.lock().unwrap().len(),
                original_configures + 1
            );
            let AudioCodec::AmrWb { mode, policy, .. } = service
                .call
                .as_ref()
                .unwrap()
                .remote
                .as_ref()
                .unwrap()
                .codec
            else {
                panic!("refresh selected the explicitly offered AMR-WB mode");
            };
            assert_eq!(mode, 2);
            assert_eq!(policy.modes().collect::<Vec<_>>(), vec![2]);
            service.handle_event(&response).await.unwrap();
            assert_eq!(
                wire.recv().await.unwrap().request.unwrap().method,
                Method::ACK
            );
            assert_eq!(
                media.configured.lock().unwrap().len(),
                original_configures + 1
            );
            service.shutdown(g()).await.unwrap();
        } else {
            assert_eq!(
                wire.recv().await.unwrap().request.unwrap().method,
                Method::BYE
            );
            assert_eq!(service.snapshot.state, "failed");
            assert_eq!(
                service.snapshot.last_error,
                "ims_session_refresh_sdp_invalid"
            );
            assert_eq!(media.configured.lock().unwrap().len(), original_configures);
            assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        }
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn failed_media_cleanup_has_no_busy_watchdog_deadline() {
    let (mut service, media, mut wire, task) = fixture();
    p5_establish(&mut service, &mut wire).await;
    *media.terminal_error.lock().unwrap() = Some(Failure::known("ims_media_decode_failed"));
    media.stop_fail.store(true, Ordering::SeqCst);
    assert!(
        service
            .tick(g(), Instant::now())
            .await
            .unwrap_err()
            .uncertain
    );
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.call.as_ref().unwrap().media_allocated);
    assert!(
        service.next_deadline().is_none(),
        "a retained cleanup fence must not spin on an expired watchdog"
    );
    media.stop_fail.store(false, Ordering::SeqCst);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn media_failure_deadline_survives_repeated_snapshot_controls() {
    let (mut service, media, mut wire, task) = fixture();
    p5_establish(&mut service, &mut wire).await;
    let deadline = service
        .next_deadline()
        .expect("active media has a watchdog");
    *media.terminal_error.lock().unwrap() = Some(Failure::known("ims_media_decode_failed"));
    // The registered consumer re-queries both snapshot and deadline after
    // each control. Traffic must not purchase a fresh watchdog interval.
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(service.snapshot().state, "active");
        assert!(service.next_deadline().unwrap() <= deadline);
    }
    service
        .tick(g(), deadline.max(Instant::now()))
        .await
        .unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot().state, "failed");
    assert!(!service.snapshot().media_ready);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_terminal_encode_and_setter_failures_after_codec_switch_bye_and_drain_once() {
    for error_code in ["ims_media_encode_failed", "ims_media_mode_change_failed"] {
        let (mut service, media, mut wire, task) = fixture();
        let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
        let mut invite = p5_offer_request(
            &call_id,
            &local_tag,
            2,
            Method::INVITE,
            p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
        );
        invite
            .data
            .headers
            .set(Header::new("Contact", "<sip:b@failed-media-contact.ims.test>").unwrap());
        service.handle_event(&request_event(invite)).await.unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
        assert_eq!(service.snapshot.codec, "AMR-WB");
        assert!(service.call.as_ref().unwrap().pending_reinvite.is_some());
        let configured = media.configured.lock().unwrap().len();
        *media.terminal_error.lock().unwrap() = Some(Failure::known(error_code));
        service.tick(g(), Instant::now()).await.unwrap();
        let bye = wire.recv().await.unwrap().request.unwrap();
        assert_eq!(bye.method, Method::BYE);
        assert_eq!(bye.uri.to_string(), "sip:b@failed-media-contact.ims.test");
        assert_eq!(service.snapshot.state, "failed");
        assert_eq!(service.snapshot.last_error, error_code);
        assert!(!service.snapshot.media_ready);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
        // The terminal report remains present; subsequent ticks cannot send
        // another BYE, restart media, or replace the original failure reason.
        service
            .tick(g(), Instant::now() + Duration::from_secs(60))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert_eq!(media.configured.lock().unwrap().len(), configured);
        assert_eq!(service.snapshot.last_error, error_code);
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn p5_update_200_and_prepare_488_retransmit_exact_cached_responses() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let accepted_request = p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::UPDATE,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    );
    service
        .handle_event(&request_event(accepted_request.clone()))
        .await
        .unwrap();
    let accepted = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(accepted.status, 200);
    let prepared = media.prepared.lock().unwrap().len();
    let configured = media.configured.lock().unwrap().len();
    service
        .handle_event(&request_event(accepted_request.clone()))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap(), accepted);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured);

    media.prepare_fail.store(true, Ordering::SeqCst);
    let rejected_request = p5_offer_request(
        &call_id,
        &local_tag,
        3,
        Method::UPDATE,
        p5_evs_offer(96, 49202),
    );
    service
        .handle_event(&request_event(rejected_request.clone()))
        .await
        .unwrap();
    let rejected = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(rejected.status, 488);
    let events = media.events.lock().unwrap().clone();
    let prepare_attempts = events
        .iter()
        .filter(|event| event.as_str() == "prepare")
        .count();
    let discards = media.discards.load(Ordering::SeqCst);
    // Disable the injected failure: a duplicate must still replay the exact
    // rejection instead of treating the retransmission as a new offer.
    media.prepare_fail.store(false, Ordering::SeqCst);
    service
        .handle_event(&request_event(rejected_request))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap(), rejected);
    service
        .handle_event(&request_event(accepted_request))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap(), accepted);
    assert_eq!(
        media
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.as_str() == "prepare")
            .count(),
        prepare_attempts
    );
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    assert_eq!(media.discards.load(Ordering::SeqCst), discards);
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(service.snapshot.codec, "AMR-WB");
    assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 2);
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_owner_answers_and_retained_offer_preserve_negotiated_telephone_events() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let body = String::from_utf8(p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"))
        .unwrap()
        .replace("RTP/AVP 111\r\n", "RTP/AVP 111 112\r\n")
        .replace(
            "a=ptime:20\r\n",
            "a=rtpmap:112 telephone-event/16000\r\na=fmtp:112 0-9\r\na=ptime:20\r\n",
        )
        .into_bytes();
    service
        .handle_event(&request_event(p5_offer_request(
            &call_id,
            &local_tag,
            2,
            Method::UPDATE,
            body,
        )))
        .await
        .unwrap();
    let answer = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(answer.status, 200);
    let answer_sdp = String::from_utf8(answer.data.body).unwrap();
    assert!(answer_sdp.contains("m=audio 49170 RTP/AVP 111 112\r\n"));
    assert!(answer_sdp.contains("a=fmtp:112 0-9\r\n"));
    assert!(!answer_sdp.contains("a=fmtp:112 0-15"));
    let configured = media.configured.lock().unwrap();
    assert_eq!(
        configured.last().unwrap().telephone_event_payload,
        Some(112)
    );
    assert_eq!(configured.last().unwrap().telephone_event_mask, 0x03ff);
    drop(configured);
    let mut offerless = in_dialog(&call_id, 3, Method::INVITE, &local_tag);
    offerless.data.body.clear();
    service
        .handle_event(&request_event(offerless))
        .await
        .unwrap();
    let local_offer = wire.recv().await.unwrap().response.unwrap();
    assert_eq!(local_offer.status, 200);
    let local_offer_sdp = String::from_utf8(local_offer.data.body).unwrap();
    assert!(local_offer_sdp.contains("a=fmtp:112 0-9\r\n"));
    assert!(!local_offer_sdp.contains("a=fmtp:112 0-15"));
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_unknown_answer_write_still_drains_when_prepared_discard_also_fails() {
    for method in [Method::INVITE, Method::UPDATE] {
        let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
        let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
        let original_media = media.configured.lock().unwrap().clone();
        media.discard_fail.store(true, Ordering::SeqCst);
        let request = p5_offer_request(
            &call_id,
            &local_tag,
            2,
            method.clone(),
            p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
        );
        let cleanup_target = contact_target(&request.data.headers)
            .unwrap()
            .unwrap_or_else(|| {
                service
                    .call
                    .as_ref()
                    .unwrap()
                    .dialog
                    .as_ref()
                    .unwrap()
                    .remote_target
                    .clone()
            });
        let event = request_event(request);
        let mut handling = Box::pin(service.handle_event(&event));
        tokio::select! {
            result = &mut handling => panic!("answer finished before writer receipt: {result:?}"),
            observed = wire.recv() => assert_eq!(observed.unwrap().response.unwrap().status, 200),
        }
        receipts
            .recv()
            .await
            .unwrap()
            .send(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "controlled answer write failure during failed discard",
            )))
            .unwrap();
        assert!(handling.await.is_err());
        assert_eq!(*media.configured.lock().unwrap(), original_media);
        assert_eq!(media.discards.load(Ordering::SeqCst), 1);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert_eq!(service.snapshot.state, "failed");
        assert!(service.snapshot.uncertain);
        assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
        assert!(!service.call.as_ref().unwrap().media_allocated);
        assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
        let prepared = media.prepared.lock().unwrap().len();
        let stop_attempts = media.stop_attempts.load(Ordering::SeqCst);
        assert!(wire.try_recv().is_err());
        if method == Method::INVITE {
            // The matching ACK proves that the uncertain 200 reached the
            // peer. Clean up this dialog once without reviving its media.
            let ack = request_event(in_dialog(&call_id, 2, Method::ACK, &local_tag));
            service.handle_event(&ack).await.unwrap();
            let bye = wire.recv().await.unwrap().request.unwrap();
            assert_eq!(bye.method, Method::BYE);
            assert_eq!(single(&bye.data.headers, "Call-ID").unwrap(), call_id);
            assert_eq!(bye.uri, cleanup_target);
            assert_eq!(
                tag(single(&bye.data.headers, "From").unwrap()),
                Some(local_tag.as_str())
            );
            assert_eq!(
                tag(single(&bye.data.headers, "To").unwrap()),
                Some("remote-tag")
            );
            assert!(service.call.as_ref().unwrap().dialog_terminated);
            service.handle_event(&ack).await.unwrap();
            service.handle_event(&event).await.unwrap();
            assert!(wire.try_recv().is_err());
        }
        // Cleanup cannot restart media or issue a second BYE at the original
        // Accepted deadline; UPDATE has no INVITE ACK cleanup transaction.
        service
            .tick(g(), Instant::now() + Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert_eq!(media.stop_attempts.load(Ordering::SeqCst), stop_attempts);
        assert_eq!(media.prepared.lock().unwrap().len(), prepared);
        assert_eq!(media.discards.load(Ordering::SeqCst), 1);
        assert_eq!(*media.configured.lock().unwrap(), original_media);
        assert_eq!(service.snapshot.state, "failed");
        assert!(service.snapshot.uncertain);
        assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
        assert!(!service.call.as_ref().unwrap().media_allocated);
        assert!(wire.try_recv().is_err());
        drop(service);
        task.await.unwrap();
    }
}

fn p5_controlled_ack_writer_fixture() -> (
    Service,
    Arc<FakeMedia>,
    Arc<AtomicBool>,
    mpsc::UnboundedReceiver<Wire>,
    mpsc::UnboundedReceiver<oneshot::Sender<std::io::Result<()>>>,
    tokio::task::JoinHandle<()>,
) {
    let (commands, mut input) = mpsc::channel(32);
    let (sent, output) = mpsc::unbounded_channel();
    let (receipts, controlled) = mpsc::unbounded_channel();
    let media = Arc::new(FakeMedia::default());
    let writer_media = media.clone();
    let control_ack = Arc::new(AtomicBool::new(false));
    let writer_control = control_ack.clone();
    let task = tokio::spawn(async move {
        while let Some(command) = input.recv().await {
            match command {
                Command::SendUntracked { request, reply, .. } => {
                    let controlled =
                        request.method == Method::ACK && writer_control.load(Ordering::SeqCst);
                    if controlled {
                        writer_media
                            .events
                            .lock()
                            .unwrap()
                            .push("ack_write_pending".into());
                    }
                    sent.send(Wire {
                        request: Some(request),
                        response: None,
                        connection: SipConnectionId::Primary,
                    })
                    .unwrap();
                    if controlled {
                        receipts.send(reply).unwrap();
                    } else {
                        let _ = reply.send(Ok(()));
                    }
                }
                Command::RespondDialog {
                    connection,
                    response,
                    reply,
                    ..
                } => {
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
                    let _ = reply.send(Ok(()));
                }
                _ => panic!("voice must not use tracked SMS transactions"),
            }
        }
    });
    let service = Service::new(g(), config(), commands, media.clone()).unwrap();
    (service, media, control_ack, output, controlled, task)
}

async fn p5_send_narrow_refresh_offer(
    service: &mut Service,
    wire: &mut mpsc::UnboundedReceiver<Wire>,
) -> Request {
    let now = Instant::now();
    let call = service.call.as_mut().unwrap();
    call.local_sdp = String::from_utf8(call.local_sdp.clone())
        .unwrap()
        .replace("mode-set=2,5", "mode-set=2")
        .into_bytes();
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
    let request = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(request.method, Method::INVITE);
    assert!(
        String::from_utf8(request.data.body.clone())
            .unwrap()
            .contains("mode-set=2;")
    );
    request
}

#[tokio::test]
async fn p5_refresh_applies_answer_and_releases_transmit_before_ack_writer_receipt() {
    let (mut service, media, control_ack, mut wire, mut receipts, task) =
        p5_controlled_ack_writer_fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    service
        .handle_event(&request_event(p5_offer_request(
            &call_id,
            &local_tag,
            2,
            Method::UPDATE,
            p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    let refresh = p5_send_narrow_refresh_offer(&mut service, &mut wire).await;
    let original_configures = media.configured.lock().unwrap().len();
    let original_prepares = media.prepared.lock().unwrap().len();
    let original_starts = media.started.load(Ordering::SeqCst);
    control_ack.store(true, Ordering::SeqCst);
    media.events.lock().unwrap().clear();
    let response = response_event(
        &refresh,
        200,
        p5_wb_offer(111, 49202, "octet-align=1;mode-set=2"),
        None,
    );
    let mut handling = Box::pin(service.handle_event(&response));
    tokio::select! {
        result = &mut handling => panic!("refresh finished before ACK writer receipt: {result:?}"),
        observed = wire.recv() => assert_eq!(observed.unwrap().request.unwrap().method, Method::ACK),
    }
    assert_eq!(
        *media.events.lock().unwrap(),
        vec![
            "holdtrue",
            "prepare",
            "configure",
            "holdfalse",
            "ack_write_pending"
        ]
    );
    assert!(!media.transmit_held.load(Ordering::SeqCst));
    assert_eq!(media.started.load(Ordering::SeqCst), original_starts);
    assert_eq!(media.prepared.lock().unwrap().len(), original_prepares + 1);
    let configured = media.configured.lock().unwrap();
    assert_eq!(configured.len(), original_configures + 1);
    assert_eq!(configured.last().unwrap().remote_port, 49202);
    let AudioCodec::AmrWb { mode, policy, .. } = configured.last().unwrap().codec else {
        panic!("received answer immediately selects the offered AMR-WB mode");
    };
    assert_eq!(mode, 2);
    assert_eq!(policy.modes().collect::<Vec<_>>(), vec![2]);
    drop(configured);
    receipts.recv().await.unwrap().send(Ok(())).unwrap();
    handling.await.unwrap();
    assert_eq!(service.snapshot.state, "active");
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
    control_ack.store(false, Ordering::SeqCst);
    service.handle_event(&response).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::ACK
    );
    assert_eq!(
        media.configured.lock().unwrap().len(),
        original_configures + 1
    );
    assert_eq!(media.prepared.lock().unwrap().len(), original_prepares + 1);
    assert_eq!(media.started.load(Ordering::SeqCst), original_starts);
    assert!(!media.transmit_held.load(Ordering::SeqCst));
    service.shutdown(g()).await.unwrap();
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_refresh_prepare_failure_keeps_transmit_held_until_ack_then_bye() {
    let (mut service, media, control_ack, mut wire, mut receipts, task) =
        p5_controlled_ack_writer_fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    service
        .handle_event(&request_event(p5_offer_request(
            &call_id,
            &local_tag,
            2,
            Method::UPDATE,
            p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
        )))
        .await
        .unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    let refresh = p5_send_narrow_refresh_offer(&mut service, &mut wire).await;
    let original_media = media.configured.lock().unwrap().clone();
    let original_starts = media.started.load(Ordering::SeqCst);
    media.prepare_fail.store(true, Ordering::SeqCst);
    control_ack.store(true, Ordering::SeqCst);
    media.events.lock().unwrap().clear();
    let response = response_event(
        &refresh,
        200,
        p5_wb_offer(111, 49202, "octet-align=1;mode-set=2"),
        None,
    );
    let mut handling = Box::pin(service.handle_event(&response));
    tokio::select! {
        result = &mut handling => panic!("refresh failure finished before ACK writer receipt: {result:?}"),
        observed = wire.recv() => assert_eq!(observed.unwrap().request.unwrap().method, Method::ACK),
    }
    let events = media.events.lock().unwrap().clone();
    assert_eq!(events.first().map(String::as_str), Some("holdtrue"));
    assert!(events.iter().any(|event| event == "prepare"));
    assert!(!events.iter().any(|event| event == "holdfalse"));
    assert!(media.transmit_held.load(Ordering::SeqCst));
    assert_eq!(*media.configured.lock().unwrap(), original_media);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    assert!(wire.try_recv().is_err());
    receipts.recv().await.unwrap().send(Ok(())).unwrap();
    let _ = handling.await;
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_media_prepare_failed");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(media.started.load(Ordering::SeqCst), original_starts);
    assert_eq!(*media.configured.lock().unwrap(), original_media);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_update_invalid_contact_is_cached_400_without_preparing_media() {
    for carries_sdp in [true, false] {
        let (mut service, media, mut wire, task) = fixture();
        let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
        let original_target = service
            .call
            .as_ref()
            .unwrap()
            .dialog
            .as_ref()
            .unwrap()
            .remote_target
            .clone();
        let original_media = media.configured.lock().unwrap().clone();
        let prepared = media.prepared.lock().unwrap().len();
        media.events.lock().unwrap().clear();
        let mut update = if carries_sdp {
            p5_offer_request(
                &call_id,
                &local_tag,
                2,
                Method::UPDATE,
                p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
            )
        } else {
            in_dialog(&call_id, 2, Method::UPDATE, &local_tag)
        };
        update
            .data
            .headers
            .set(Header::new("Contact", "<sip:b@missing-close.ims.test").unwrap());
        service
            .handle_event(&request_event(update.clone()))
            .await
            .unwrap();
        let rejected = wire.recv().await.unwrap().response.unwrap();
        assert_eq!(rejected.status, 400);
        service.handle_event(&request_event(update)).await.unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap(), rejected);
        assert_eq!(media.prepared.lock().unwrap().len(), prepared);
        assert_eq!(*media.configured.lock().unwrap(), original_media);
        assert!(
            !media
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event == "prepare")
        );
        assert_eq!(
            service
                .call
                .as_ref()
                .unwrap()
                .dialog
                .as_ref()
                .unwrap()
                .remote_target,
            original_target
        );
        assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 1);
        assert_eq!(service.snapshot.state, "active");
        service.shutdown(g()).await.unwrap();
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn p5_update_accepted_contact_is_used_by_next_bye_with_or_without_sdp() {
    for carries_sdp in [true, false] {
        let (mut service, media, mut wire, task) = fixture();
        let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
        let mut update = if carries_sdp {
            p5_offer_request(
                &call_id,
                &local_tag,
                2,
                Method::UPDATE,
                p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
            )
        } else {
            in_dialog(&call_id, 2, Method::UPDATE, &local_tag)
        };
        update
            .data
            .headers
            .set(Header::new("Contact", "<sip:b@accepted-update.ims.test>").unwrap());
        service
            .handle_event(&request_event(update.clone()))
            .await
            .unwrap();
        let accepted = wire.recv().await.unwrap().response.unwrap();
        assert_eq!(accepted.status, 200);
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
            "sip:b@accepted-update.ims.test"
        );
        let prepared = media.prepared.lock().unwrap().len();
        let configured = media.configured.lock().unwrap().len();
        service.handle_event(&request_event(update)).await.unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap(), accepted);
        assert_eq!(media.prepared.lock().unwrap().len(), prepared);
        assert_eq!(media.configured.lock().unwrap().len(), configured);
        service.hangup(g()).await.unwrap();
        let bye = wire.recv().await.unwrap().request.unwrap();
        assert_eq!(bye.method, Method::BYE);
        assert_eq!(bye.uri.to_string(), "sip:b@accepted-update.ims.test");
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        drop(service);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn p5_reinvite_ack_does_not_restore_contact_before_newer_bodyless_update() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let mut invite = p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::INVITE,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    );
    invite
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@contact-a.ims.test>").unwrap());
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
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
        "sip:b@contact-a.ims.test"
    );
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_some());
    let prepared = media.prepared.lock().unwrap().len();
    let configured = media.configured.lock().unwrap().len();
    let mut update = in_dialog(&call_id, 3, Method::UPDATE, &local_tag);
    assert!(update.data.body.is_empty());
    update
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@contact-b.ims.test>").unwrap());
    service.handle_event(&request_event(update)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
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
        "sip:b@contact-b.ims.test"
    );
    service
        .handle_event(&request_event(in_dialog(
            &call_id,
            2,
            Method::ACK,
            &local_tag,
        )))
        .await
        .unwrap();
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
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
        "sip:b@contact-b.ims.test"
    );
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    service.hangup(g()).await.unwrap();
    let bye = wire.recv().await.unwrap().request.unwrap();
    assert_eq!(bye.method, Method::BYE);
    assert_eq!(bye.uri.to_string(), "sip:b@contact-b.ims.test");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_terminal_failure_with_stop_error_terminalizes_once_and_retains_cleanup_fence() {
    let (mut service, media, mut wire, task) = fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    // Retain both re-INVITE and session deadlines. A failed stop must fence
    // media and disarm refresh/expiry BYEs; the independent Accepted TU may
    // still retransmit its known 200 until ACK or its original deadline.
    let mut invite = p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::INVITE,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    );
    push(
        &mut invite.data.headers,
        "Session-Expires",
        "180;refresher=uac",
    )
    .unwrap();
    service.handle_event(&request_event(invite)).await.unwrap();
    assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 200);
    let pending_deadline = service
        .call
        .as_ref()
        .unwrap()
        .pending_reinvite
        .as_ref()
        .unwrap()
        .deadline;
    let now = Instant::now();
    service.call.as_mut().unwrap().session_timer = Some(SessionTimer {
        interval: 180,
        min_se: 90,
        local_refresher: true,
        expires_at: now + Duration::from_secs(180),
        termination_at: now + Duration::from_secs(180),
        refresh_at: Some(now + Duration::from_secs(90)),
        pending: None,
        retry_count: 0,
    });
    *media.terminal_error.lock().unwrap() = Some(Failure::known("ims_media_encode_failed"));
    media.stop_fail.store(true, Ordering::SeqCst);
    let stop_failure = service.tick(g(), now).await.unwrap_err();
    assert_eq!(stop_failure.code, "ims_media_stop_failed");
    assert!(stop_failure.uncertain);
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert!(!service.snapshot.media_ready);
    assert!(!service.snapshot.early_media);
    assert_eq!(service.snapshot.audio_port, 0);
    assert!(service.snapshot.audio_token.is_empty());
    assert_eq!(service.snapshot.last_error, "ims_media_encode_failed");
    assert_eq!(media.stop_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 0);
    let call = service.call.as_ref().unwrap();
    assert!(call.media_allocated);
    assert!(call.uncertain);
    assert!(call.pending_reinvite.is_none());
    assert!(call.session_timer.is_none());
    assert!(call.deadline.is_none());
    assert!(service.active());
    let allocated = media.allocated.load(Ordering::SeqCst);
    assert!(service.dial(g(), "+447700900124").await.is_err());
    assert_eq!(media.allocated.load(Ordering::SeqCst), allocated);
    assert!(wire.try_recv().is_err());
    // The same persistent terminal media report must not trigger another BYE
    // or revive a SIP timer while resources remain fenced for cleanup.
    let _ = service
        .tick(g(), pending_deadline + Duration::from_secs(300))
        .await;
    assert!(wire.try_recv().is_err());
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert_eq!(service.snapshot.last_error, "ims_media_encode_failed");
    assert!(service.call.as_ref().unwrap().media_allocated);
    let observed = Arc::new(Mutex::new(Vec::<Snapshot>::new()));
    let observer_output = observed.clone();
    service.set_observer(Arc::new(move |_, snapshot| {
        observer_output.lock().unwrap().push(snapshot);
    }));
    assert!(service.shutdown(g()).await.unwrap_err().uncertain);
    assert!(!service.snapshot.signaling_ready);
    assert!(!observed.lock().unwrap().last().unwrap().signaling_ready);
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert!(service.call.as_ref().unwrap().media_allocated);
    assert_eq!(service.snapshot.last_error, "ims_media_encode_failed");
    assert!(wire.try_recv().is_err());
    media.stop_fail.store(false, Ordering::SeqCst);
    service.shutdown(g()).await.unwrap();
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert!(media.stop_attempts.load(Ordering::SeqCst) >= 2);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    assert!(!service.call.as_ref().unwrap().media_started);
    assert_eq!(service.snapshot.last_error, "ims_media_encode_failed");
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

fn p5_session_timer_facts(
    service: &Service,
) -> Option<(u32, u32, bool, Instant, Instant, Option<Instant>, bool, u8)> {
    service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .map(|timer| {
            (
                timer.interval,
                timer.min_se,
                timer.local_refresher,
                timer.expires_at,
                timer.termination_at,
                timer.refresh_at,
                timer.pending.is_some(),
                timer.retry_count,
            )
        })
}

#[tokio::test]
async fn p5_reinvite_ack_preserves_timer_effective_at_latest_successful_response() {
    // 0: retain timer A; 1: accept newer timer B; 2: disable expiration through
    // a later 200 without Session-Expires. ACK cannot change any of them.
    for offerless in [false, true] {
        for later_update in [0, 1, 2] {
            let (mut service, _media, mut wire, task) = fixture();
            let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
            assert!(service.call.as_ref().unwrap().session_timer.is_none());
            let mut invite = if offerless {
                let mut request = in_dialog(&call_id, 2, Method::INVITE, &local_tag);
                request.data.body.clear();
                request
            } else {
                p5_offer_request(
                    &call_id,
                    &local_tag,
                    2,
                    Method::INVITE,
                    p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
                )
            };
            push(&mut invite.data.headers, "Supported", "timer").unwrap();
            push(
                &mut invite.data.headers,
                "Session-Expires",
                "180;refresher=uas",
            )
            .unwrap();
            push(&mut invite.data.headers, "Min-SE", "90").unwrap();
            let before_200 = Instant::now();
            service.handle_event(&request_event(invite)).await.unwrap();
            let accepted = wire.recv().await.unwrap().response.unwrap();
            let after_200 = Instant::now();
            assert_eq!(accepted.status, 200);
            assert_eq!(
                single(&accepted.data.headers, "Session-Expires").unwrap(),
                "180;refresher=uas"
            );
            assert!(service.call.as_ref().unwrap().pending_reinvite.is_some());
            assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 1);
            let timer_a = service
                .call
                .as_ref()
                .unwrap()
                .session_timer
                .as_ref()
                .expect("accepted 200 immediately establishes timer A before ACK");
            assert_eq!(timer_a.interval, 180);
            assert_eq!(timer_a.min_se, 90);
            assert!(timer_a.local_refresher);
            assert!(timer_a.refresh_at.is_some());
            assert!(timer_a.expires_at >= before_200 + Duration::from_secs(180));
            assert!(timer_a.expires_at <= after_200 + Duration::from_secs(180));
            let facts_a = p5_session_timer_facts(&service);

            if later_update != 0 {
                let mut update = in_dialog(&call_id, 3, Method::UPDATE, &local_tag);
                assert!(update.data.body.is_empty());
                push(&mut update.data.headers, "Supported", "timer").unwrap();
                if later_update == 1 {
                    push(
                        &mut update.data.headers,
                        "Session-Expires",
                        "300;refresher=uac",
                    )
                    .unwrap();
                    push(&mut update.data.headers, "Min-SE", "240").unwrap();
                }
                service.handle_event(&request_event(update)).await.unwrap();
                let updated = wire.recv().await.unwrap().response.unwrap();
                assert_eq!(updated.status, 200);
                if later_update == 1 {
                    assert_eq!(
                        single(&updated.data.headers, "Session-Expires").unwrap(),
                        "300;refresher=uac"
                    );
                    let timer_b = service
                        .call
                        .as_ref()
                        .unwrap()
                        .session_timer
                        .as_ref()
                        .unwrap();
                    assert_eq!(timer_b.interval, 300);
                    assert_eq!(timer_b.min_se, 240);
                    assert!(!timer_b.local_refresher);
                    assert!(timer_b.refresh_at.is_none());
                    assert_ne!(p5_session_timer_facts(&service), facts_a);
                } else {
                    assert!(updated.data.headers.get("Session-Expires").is_none());
                    assert!(service.call.as_ref().unwrap().session_timer.is_none());
                }
            }
            let before_ack = p5_session_timer_facts(&service);
            let ack = if offerless {
                p5_offer_request(
                    &call_id,
                    &local_tag,
                    2,
                    Method::ACK,
                    p5_evs_offer(96, 49202),
                )
            } else {
                in_dialog(&call_id, 2, Method::ACK, &local_tag)
            };
            service
                .handle_event(&request_event(ack.clone()))
                .await
                .unwrap();
            assert_eq!(service.snapshot.state, "active");
            assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
            assert_eq!(service.call.as_ref().unwrap().local_sdp_version, 2);
            assert_eq!(p5_session_timer_facts(&service), before_ack);
            service.handle_event(&request_event(ack)).await.unwrap();
            assert_eq!(p5_session_timer_facts(&service), before_ack);
            assert!(wire.try_recv().is_err());
            service.shutdown(g()).await.unwrap();
            drop(service);
            task.await.unwrap();
        }
    }
}

#[tokio::test]
async fn p5_update_sdp_session_timer_starts_at_successful_answer_writer_receipt() {
    let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let mut update = p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::UPDATE,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    );
    push(&mut update.data.headers, "Supported", "timer").unwrap();
    push(
        &mut update.data.headers,
        "Session-Expires",
        "90;refresher=uac",
    )
    .unwrap();
    push(&mut update.data.headers, "Min-SE", "90").unwrap();
    let event = request_event(update);
    let configured = media.configured.lock().unwrap().len();
    let mut handling = Box::pin(service.handle_event(&event));
    tokio::select! {
        result = &mut handling => panic!("UPDATE finished before answer writer receipt: {result:?}"),
        observed = wire.recv() => {
            let response = observed.unwrap().response.unwrap();
            assert_eq!(response.status, 200);
            assert_eq!(single(&response.data.headers, "Session-Expires").unwrap(), "90;refresher=uac");
        }
    }
    let receipt = receipts.recv().await.unwrap();
    assert_eq!(media.configured.lock().unwrap().len(), configured);
    // The crate intentionally has no tokio test-util dependency. Hold the
    // writer briefly in real time, then use the owner's explicit Instant tick
    // to exercise the expiration boundary without waiting a session interval.
    tokio::time::sleep(Duration::from_millis(40)).await;
    let receipt_released_at = Instant::now();
    receipt.send(Ok(())).unwrap();
    handling.await.unwrap();
    let response_completed_at = Instant::now();
    let timer = service
        .call
        .as_ref()
        .unwrap()
        .session_timer
        .as_ref()
        .unwrap();
    assert_eq!(timer.interval, 90);
    assert_eq!(timer.min_se, 90);
    assert!(!timer.local_refresher);
    assert!(timer.refresh_at.is_none());
    assert!(timer.expires_at >= receipt_released_at + Duration::from_secs(90));
    assert!(timer.expires_at <= response_completed_at + Duration::from_secs(90));
    assert!(timer.termination_at >= receipt_released_at + Duration::from_secs(60));
    let termination_at = timer.termination_at;
    assert_eq!(service.snapshot.state, "active");
    assert_eq!(media.configured.lock().unwrap().len(), configured + 1);
    service
        .tick(g(), termination_at - Duration::from_millis(10))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "active");
    assert!(wire.try_recv().is_err());
    service.tick(g(), termination_at).await.unwrap();
    assert_eq!(
        wire.recv().await.unwrap().request.unwrap().method,
        Method::BYE
    );
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, "ims_session_expired");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_bodyless_update_unknown_200_write_fences_pending_dialog_and_later_updates() {
    let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
    let (call_id, local_tag) = p5_establish(&mut service, &mut wire).await;
    let mut invite = p5_offer_request(
        &call_id,
        &local_tag,
        2,
        Method::INVITE,
        p5_wb_offer(111, 49200, "octet-align=1;mode-set=2,5"),
    );
    push(
        &mut invite.data.headers,
        "Session-Expires",
        "180;refresher=uac",
    )
    .unwrap();
    let invite_event = request_event(invite);
    let mut accepting = Box::pin(service.handle_event(&invite_event));
    tokio::select! {
        result = &mut accepting => panic!("re-INVITE finished without 200 receipt: {result:?}"),
        observed = wire.recv() => assert_eq!(observed.unwrap().response.unwrap().status, 200),
    }
    receipts.recv().await.unwrap().send(Ok(())).unwrap();
    accepting.await.unwrap();
    assert!(service.call.as_ref().unwrap().pending_reinvite.is_some());
    assert!(service.call.as_ref().unwrap().session_timer.is_some());
    let configured = media.configured.lock().unwrap().clone();
    let prepared = media.prepared.lock().unwrap().len();
    let original_target = service
        .call
        .as_ref()
        .unwrap()
        .dialog
        .as_ref()
        .unwrap()
        .remote_target
        .clone();
    let mut update = in_dialog(&call_id, 3, Method::UPDATE, &local_tag);
    update
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@unconfirmed-update.ims.test>").unwrap());
    push(
        &mut update.data.headers,
        "Session-Expires",
        "300;refresher=uac",
    )
    .unwrap();
    let update_event = request_event(update.clone());
    let mut handling = Box::pin(service.handle_event(&update_event));
    tokio::select! {
        result = &mut handling => panic!("UPDATE finished without 200 receipt: {result:?}"),
        observed = wire.recv() => assert_eq!(observed.unwrap().response.unwrap().status, 200),
    }
    receipts
        .recv()
        .await
        .unwrap()
        .send(Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "controlled bodyless UPDATE write failure",
        )))
        .unwrap();
    let failure = handling.await.unwrap_err();
    assert!(failure.uncertain);
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    let call = service.call.as_ref().unwrap();
    assert!(call.uncertain);
    assert!(!call.media_allocated);
    assert!(call.session_timer.is_none());
    assert!(call.pending_reinvite.is_none());
    assert!(call.deadline.is_none());
    assert_eq!(call.dialog.as_ref().unwrap().remote_target, original_target);
    // A retransmission of the failed UPDATE and new UPDATE/PRACK attempts must
    // be rejected before cache replay, target refresh or timer negotiation.
    let mut next_update = in_dialog(&call_id, 4, Method::UPDATE, &local_tag);
    next_update
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@must-not-be-accepted.ims.test>").unwrap());
    push(
        &mut next_update.data.headers,
        "Session-Expires",
        "600;refresher=uac",
    )
    .unwrap();
    let mut prack = p5_offer_request(
        &call_id,
        &local_tag,
        5,
        Method::PRACK,
        p5_wb_offer(111, 49204, "octet-align=1;mode-set=2,5"),
    );
    prack
        .data
        .headers
        .set(Header::new("Contact", "<sip:b@must-not-be-accepted.ims.test>").unwrap());
    // Keep this a well-formed but unassociated PRACK: malformed RAck now has
    // its independent 400 response before any SDP or dialog-state mutation.
    push(&mut prack.data.headers, "RAck", "1 1 INVITE").unwrap();
    for request in [update, next_update, prack] {
        service.handle_event(&request_event(request)).await.unwrap();
        assert_eq!(wire.recv().await.unwrap().response.unwrap().status, 481);
        assert!(service.call.as_ref().unwrap().session_timer.is_none());
        assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
        assert_eq!(
            service
                .call
                .as_ref()
                .unwrap()
                .dialog
                .as_ref()
                .unwrap()
                .remote_target,
            original_target
        );
    }
    service
        .handle_event(&request_event(in_dialog(
            &call_id,
            2,
            Method::ACK,
            &local_tag,
        )))
        .await
        .unwrap();
    service
        .tick(g(), Instant::now() + Duration::from_secs(600))
        .await
        .unwrap();
    assert_eq!(service.snapshot.state, "failed");
    assert!(service.snapshot.uncertain);
    assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
    assert_eq!(*media.configured.lock().unwrap(), configured);
    assert_eq!(media.prepared.lock().unwrap().len(), prepared);
    assert!(wire.try_recv().is_err());
    drop(service);
    task.await.unwrap();
}

#[tokio::test]
async fn p5_late_initial_ack_cannot_revive_incoming_call_after_bodyless_update_write_failure() {
    for timeout_first in [false, true] {
        let (mut service, media, mut wire, mut receipts, task) = p5_controlled_writer_fixture();
        let call_id = "p5-incoming-unconfirmed-update";
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
        let local_tag = service.call.as_ref().unwrap().local_tag.clone();
        let mut answering = Box::pin(service.answer(g()));
        tokio::select! {
            _ = &mut answering => panic!("initial answer finished without 200 receipt"),
            observed = wire.recv() => assert_eq!(observed.unwrap().response.unwrap().status, 200),
        }
        assert_eq!(media.prepared.lock().unwrap().len(), 1);
        assert!(media.configured.lock().unwrap().is_empty());
        assert!(media.start_modes.lock().unwrap().is_empty());
        receipts.recv().await.unwrap().send(Ok(())).unwrap();
        answering.await.unwrap();
        assert_eq!(service.snapshot.state, "connecting");
        assert!(service.call.as_ref().unwrap().session_timer.is_some());
        assert_eq!(media.started.load(Ordering::SeqCst), 1);
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
        assert_eq!(media.prepared.lock().unwrap().len(), 1);
        assert_eq!(media.configured.lock().unwrap().len(), 1);
        assert!(service.snapshot.media_ready);
        assert!(service.snapshot.early_media);
        assert!(service.snapshot.started_at.is_empty());
        let configured = media.configured.lock().unwrap().clone();
        let initial_deadline = service
            .call
            .as_ref()
            .unwrap()
            .initial_answer
            .as_ref()
            .unwrap()
            .deadline;
        let mut update = inbound(call_id, 15, Method::UPDATE, Some(&local_tag));
        push(
            &mut update.data.headers,
            "Session-Expires",
            "300;refresher=uac",
        )
        .unwrap();
        let update_event = request_event(update);
        let mut handling = Box::pin(service.handle_event(&update_event));
        tokio::select! {
            result = &mut handling => panic!("UPDATE finished without 200 receipt: {result:?}"),
            observed = wire.recv() => assert_eq!(observed.unwrap().response.unwrap().status, 200),
        }
        receipts
            .recv()
            .await
            .unwrap()
            .send(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "controlled incoming bodyless UPDATE failure",
            )))
            .unwrap();
        assert!(handling.await.unwrap_err().uncertain);
        assert_eq!(service.snapshot.state, "failed");
        assert!(service.snapshot.uncertain);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        assert!(service.call.as_ref().unwrap().session_timer.is_none());
        assert!(service.call.as_ref().unwrap().pending_reinvite.is_none());
        assert!(
            wire.try_recv().is_err(),
            "failed local update must await initial ACK or deadline before BYE"
        );
        if timeout_first {
            service.tick(g(), initial_deadline).await.unwrap();
            let bye = wire.recv().await.unwrap().request.unwrap();
            assert_eq!(bye.method, Method::BYE);
            assert_eq!(single(&bye.data.headers, "Call-ID").unwrap(), call_id);
            assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
            assert!(wire.try_recv().is_err());
        }
        let late_ack = inbound(call_id, 14, Method::ACK, Some(&local_tag));
        for attempt in 0..2 {
            service
                .handle_event(&request_event(late_ack.clone()))
                .await
                .unwrap();
            assert_eq!(service.snapshot.state, "failed");
            assert!(service.snapshot.uncertain);
            assert!(!service.snapshot.media_ready);
            assert_eq!(media.started.load(Ordering::SeqCst), 1);
            assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
            assert_eq!(*media.configured.lock().unwrap(), configured);
            assert_eq!(media.prepared.lock().unwrap().len(), 1);
            assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
            assert!(service.snapshot.started_at.is_empty());
            if !timeout_first && attempt == 0 {
                let bye = wire.recv().await.unwrap().request.unwrap();
                assert_eq!(bye.method, Method::BYE);
                assert_eq!(single(&bye.data.headers, "Call-ID").unwrap(), call_id);
            }
            assert!(
                wire.try_recv().is_err(),
                "failed initial dialog sends BYE exactly once and ACK cannot reopen media"
            );
        }
        service
            .tick(g(), initial_deadline + Duration::from_secs(1))
            .await
            .unwrap();
        assert!(wire.try_recv().is_err());
        assert_eq!(service.snapshot.state, "failed");
        assert_eq!(service.snapshot.last_error, "ims_call_write_unconfirmed");
        assert_eq!(*media.start_modes.lock().unwrap(), vec![true]);
        assert_eq!(*media.configured.lock().unwrap(), configured);
        assert_eq!(media.stopped.load(Ordering::SeqCst), 1);
        drop(service);
        task.await.unwrap();
    }
}
