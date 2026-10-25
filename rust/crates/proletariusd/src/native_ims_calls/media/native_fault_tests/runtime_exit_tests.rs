//! Real post-ready thread exits, with native allocation accounting and actual
//! tunneled UDP/PCM. No process OOM, foreign signal or production fault switch.
use super::*;

async fn stopped(owner: &Owner, binding: &CallBinding) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(error) = owner.terminal_failure(binding) {
                assert_eq!(error.code, CODEC_STOPPED);
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("post-ready exit must wake idle media without RTP/PCM");
}
fn exit_worker(record: &WorkerRecord, panic: bool) {
    record.action(move || {
        if panic {
            panic!("controlled post-ready actual codec worker panic");
        }
        false
    });
}
async fn voice(
    network: &mut NetworkPair,
    local: &LocalMedia,
    binding: &CallBinding,
    codec: AudioCodec,
) {
    let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let prefix = format!("{}\0", local.audio_token).into_bytes();
    let bridge = SocketAddr::from(([127, 0, 0, 1], local.audio_port));
    let mut peer = CodecWorker::new(codec).await.unwrap();
    let mut peer_speech = false;
    let mut host_speech = false;
    let mut previous: Option<(u32, u16, u32)> = None;
    let before = network.owner.statistics(binding);
    for index in 0..4 {
        let mut pcm = prefix.clone();
        pcm.extend(voiced(index));
        host.send_to(&pcm, bridge).await.unwrap();
        let packet = tokio::time::timeout(Duration::from_secs(3), network.remote.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(packet.peer, SocketAddr::new(local.ip, local.port));
        let parsed = RtpPacket::parse(&packet.bytes).unwrap();
        assert_eq!(
            parsed.payload_type,
            if matches!(codec, AudioCodec::Evs { .. }) {
                96
            } else {
                98
            }
        );
        if let Some((ssrc, seq, ts)) = previous {
            assert_eq!(
                (parsed.ssrc, parsed.sequence, parsed.timestamp),
                (ssrc, seq.wrapping_add(1), ts.wrapping_add(320))
            );
        }
        previous = Some((parsed.ssrc, parsed.sequence, parsed.timestamp));
        peer_speech |= peer
            .decode(parsed.payload)
            .await
            .unwrap()
            .iter()
            .any(|byte| *byte != 0);
        network
            .remote
            .sender()
            .send_to(packet.bytes.to_vec(), SocketAddr::new(local.ip, local.port))
            .await
            .unwrap();
        let mut output = [0u8; 2048];
        let (len, source) =
            tokio::time::timeout(Duration::from_secs(3), host.recv_from(&mut output))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(source, bridge);
        assert_eq!(len, prefix.len() + 640);
        assert_eq!(&output[..prefix.len()], prefix.as_slice());
        host_speech |= output[prefix.len()..len].iter().any(|byte| *byte != 0);
    }
    assert!(peer_speech && host_speech);
    let after = network.owner.statistics(binding);
    assert_eq!(after.rtp_packets_sent, before.rtp_packets_sent + 4);
    assert_eq!(after.rtp_packets_received, before.rtp_packets_received + 4);
    assert_eq!(after.audio_frames_to_host, before.audio_frames_to_host + 4);
    assert_eq!(after.rtp_decode_errors, 0);
    peer.close().await.unwrap();
}

#[tokio::test]
async fn idle_post_ready_panic_or_exit_revokes_media_and_keeps_ports_until_join() {
    for codec in [evs(), wb()] {
        for panic in [false, true] {
            let plan = Arc::new(FaultPlan::default());
            let _release = ReleaseOnDrop(plan.clone());
            plan.arm(1, None, false, true);
            let mut network = NetworkPair::new(plan.clone()).await;
            let binding = network.binding("idle-post-ready-exit");
            let local = network
                .owner
                .clone()
                .allocate(binding.clone())
                .await
                .unwrap();
            network
                .owner
                .clone()
                .configure(binding.clone(), network.negotiation(codec))
                .await
                .unwrap();
            network
                .owner
                .clone()
                .start(binding.clone(), true)
                .await
                .unwrap();
            let worker = plan.worker(1);
            exit_worker(&worker, panic);
            worker.wait_exit_gate().await;
            stopped(&network.owner, &binding).await;
            assert_eq!(network.owner.statistics(&binding).rtp_packets_received, 0);
            assert_eq!(network.owner.statistics(&binding).rtp_packets_sent, 0);
            assert!(
                network
                    .owner
                    .clone()
                    .start(binding.clone(), false)
                    .await
                    .is_err()
            );
            assert!(
                network
                    .owner
                    .clone()
                    .dtmf(binding.clone(), '#')
                    .await
                    .is_err()
            );
            let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let mut pcm = format!("{}\0", local.audio_token).into_bytes();
            pcm.extend(voiced(1));
            host.send_to(&pcm, SocketAddr::from(([127, 0, 0, 1], local.audio_port)))
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(30), network.remote.recv())
                    .await
                    .is_err()
            );
            assert!(
                network
                    .owner
                    .clone()
                    .allocate(network.binding("too-early-new-call"))
                    .await
                    .is_err()
            );
            let mut stopping = Box::pin(network.owner.clone().stop(binding));
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut stopping)
                    .await
                    .is_err()
            );
            assert!(
                UdpSocket::bind(("127.0.0.1", local.audio_port))
                    .await
                    .is_err()
            );
            worker.release_exit();
            tokio::time::timeout(Duration::from_secs(3), stopping)
                .await
                .unwrap()
                .unwrap();
            plan.assert_drained();
            let reclaimed = UdpSocket::bind(("127.0.0.1", local.audio_port))
                .await
                .unwrap();
            drop(reclaimed);
            let retry = network.binding("post-ready-cleanup-confirmed-new-call");
            let retry_local = network.owner.clone().allocate(retry.clone()).await.unwrap();
            assert_ne!(retry_local.audio_token, local.audio_token);
            network
                .owner
                .clone()
                .configure(retry.clone(), network.negotiation(codec))
                .await
                .unwrap();
            network
                .owner
                .clone()
                .start(retry.clone(), false)
                .await
                .unwrap();
            voice(&mut network, &retry_local, &retry, codec).await;
            network.owner.clone().stop(retry).await.unwrap();
            plan.assert_drained();
            network.finish().await;
        }
    }
}

#[tokio::test]
async fn receive_ready_propagates_disconnected_worker_instead_of_packet_loss_plc() {
    for panic in [false, true] {
        let plan = Arc::new(FaultPlan::default());
        let _release = ReleaseOnDrop(plan.clone());
        plan.arm(1, None, false, true);
        let mut media = NegotiationState::default();
        media.native_fault = Some(plan.clone());
        let remote = AudioNegotiation {
            remote_ip: "10.0.0.3".parse().unwrap(),
            remote_port: 4000,
            payload_type: 96,
            codec: evs(),
            telephone_event_payload: Some(102),
            telephone_event_mask: 0xffff,
        };
        media.configure(remote).await.unwrap();
        let mut peer = CodecWorker::new(evs()).await.unwrap();
        let payload = peer.encode(&voiced(1)).await.unwrap();
        let receiver = media.current.as_mut().unwrap();
        receiver.ingress.lock().unwrap().push(
            receiver.id,
            &RtpPacket {
                marker: false,
                payload_type: 96,
                sequence: 100,
                timestamp: 32000,
                ssrc: 573,
                payload: &payload,
            },
            std::time::Instant::now(),
        );
        let worker = plan.worker(1);
        exit_worker(&worker, panic);
        worker.wait_exit_gate().await;
        let audio = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let stats = Mutex::new(Stats::default());
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            receive_ready(&mut media, &audio, b"test-token\0", None, &stats),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.code, CODEC_STOPPED);
        assert_eq!(stats.lock().unwrap().rtp_packets_received, 1);
        assert_eq!(
            stats.lock().unwrap().rtp_decode_errors,
            0,
            "thread death must not masquerade as malformed RTP"
        );
        let mut closing = Box::pin(media.close());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut closing)
                .await
                .is_err()
        );
        worker.release_exit();
        tokio::time::timeout(Duration::from_secs(3), closing)
            .await
            .unwrap()
            .unwrap();
        peer.close().await.unwrap();
        plan.assert_drained();
    }
}

struct Pause {
    entered: tokio::sync::Notify,
    waiting: AtomicBool,
    release: (Mutex<bool>, Condvar),
}
impl Pause {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: tokio::sync::Notify::new(),
            waiting: AtomicBool::new(false),
            release: (Mutex::new(false), Condvar::new()),
        })
    }
    fn hold(&self) {
        self.waiting.store(true, Ordering::SeqCst);
        self.entered.notify_waiters();
        let mut open = self.release.0.lock().unwrap();
        while !*open {
            open = self.release.1.wait(open).unwrap();
        }
    }
    fn open(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
    }
    async fn waiting(&self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let changed = self.entered.notified();
                if self.waiting.load(Ordering::SeqCst) {
                    break;
                }
                changed.await;
            }
        })
        .await
        .unwrap();
    }
}
struct OpenOnDrop(Arc<Pause>);
impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        self.0.open();
    }
}

#[tokio::test]
async fn full_actual_native_command_queue_is_recoverable_and_not_terminal() {
    let plan = Arc::new(FaultPlan::default());
    let _release = ReleaseOnDrop(plan.clone());
    let mut worker = scope(plan.clone(), CodecWorker::new(evs())).await.unwrap();
    let pause = Pause::new();
    let _open = OpenOnDrop(pause.clone());
    let in_thread = pause.clone();
    plan.worker(1).action(move || {
        in_thread.hold();
        true
    });
    pause.waiting().await;
    let mut peer = CodecWorker::new(evs()).await.unwrap();
    let packet = peer.encode(&voiced(1)).await.unwrap();
    let (bits, mode, lost, _) = CodecWorker::frame(evs(), &packet).unwrap();
    let mut replies = vec![];
    for _ in 0..8 {
        let (reply, result) = oneshot::channel();
        assert!(
            worker
                .native
                .as_ref()
                .unwrap()
                .sender
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .try_send(CodecCommand::Decode(bits.clone(), mode, lost, reply))
                .is_ok()
        );
        replies.push(result);
    }
    let error = worker.encode(&voiced(2)).await.unwrap_err();
    assert_eq!(error.code, "ims_media_codec_busy");
    assert!(
        worker
            .native
            .as_ref()
            .unwrap()
            .health
            .media
            .failure()
            .is_none()
    );
    pause.open();
    for reply in replies {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), reply)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .len(),
            640
        );
    }
    assert!(!worker.encode(&voiced(3)).await.unwrap().is_empty());
    assert!(
        worker
            .native
            .as_ref()
            .unwrap()
            .health
            .media
            .failure()
            .is_none()
    );
    peer.close().await.unwrap();
    worker.close().await.unwrap();
    plan.assert_drained();
}

// This writer acknowledges actual registered-session command receipts. The
// media implementation below is always the real tunneled Owner, never FakeMedia.
enum SipWire {
    Response(Response),
    Request(Request),
}
fn sip_writer(
    hold_bye: bool,
    hold_update: bool,
    hold_prack: bool,
) -> (
    mpsc::Sender<Command>,
    mpsc::UnboundedReceiver<SipWire>,
    tokio::task::JoinHandle<()>,
    oneshot::Sender<()>,
    oneshot::Sender<()>,
    oneshot::Sender<()>,
) {
    let (commands, mut input) = mpsc::channel(32);
    let (wire, output) = mpsc::unbounded_channel();
    let (release, mut released) = oneshot::channel();
    let (release_update, mut update_released) = oneshot::channel();
    let (release_prack, prack_released) = oneshot::channel();
    let mut prack_released = Some(prack_released);
    let task = tokio::spawn(async move {
        while let Some(command) = input.recv().await {
            match command {
                Command::RespondDialog {
                    response, reply, ..
                } => {
                    let update = response.status == 200
                        && matches!(cseq(&response.data.headers), Ok((_, Method::UPDATE)));
                    let prack = response.status == 200
                        && matches!(cseq(&response.data.headers), Ok((_, Method::PRACK)));
                    wire.send(SipWire::Response(response)).unwrap();
                    if hold_prack
                        && prack
                        && let Some(released) = prack_released.take()
                    {
                        let _ = released.await;
                    }
                    if hold_update && update {
                        let _ = (&mut update_released).await;
                    }
                    reply.send(Ok(())).unwrap();
                }
                Command::SendUntracked { request, reply, .. } => {
                    let bye = request.method == Method::BYE;
                    wire.send(SipWire::Request(request)).unwrap();
                    if hold_bye && bye {
                        let _ = (&mut released).await;
                    }
                    reply.send(Ok(())).unwrap();
                }
                _ => panic!("native runtime test must not borrow tracked SIP transactions"),
            }
        }
    });
    (
        commands,
        output,
        task,
        release,
        release_update,
        release_prack,
    )
}
async fn sip_wire(output: &mut mpsc::UnboundedReceiver<SipWire>) -> SipWire {
    tokio::time::timeout(Duration::from_secs(3), output.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn sip_response(output: &mut mpsc::UnboundedReceiver<SipWire>, status: u16) -> Response {
    let SipWire::Response(response) = sip_wire(output).await else {
        panic!("expected SIP response")
    };
    assert_eq!(response.status, status);
    response
}
fn offered_invite(network: &NetworkPair, id: &str, codec: AudioCodec, reliable: bool) -> Request {
    let mut invite = offerless_invite(id);
    let remote = network.negotiation(codec);
    // The builder's selected branch is needed for a single-codec carrier
    // offer. Its generic-offer branch always advertises EVS+WB and ignores
    // codec/PT arguments, which would correctly make Service prefer EVS.
    invite.data.body = sdp::build_local_sdp(
        573,
        remote.remote_ip,
        remote.remote_port,
        true,
        Some(codec),
        Some(remote.payload_type),
        Some(102),
        false,
    )
    .unwrap()
    .into_bytes();
    push(&mut invite.data.headers, "Content-Type", "application/sdp").unwrap();
    if reliable {
        push(&mut invite.data.headers, "Require", "100rel").unwrap();
    }
    invite
}
fn offered_ack(invite: &Request, response: &Response) -> Request {
    let mut ack = invite.clone();
    ack.method = Method::ACK;
    ack.data.body.clear();
    ack.data.headers.set(Header::new("CSeq", "1 ACK").unwrap());
    ack.data
        .headers
        .set(Header::new("To", single(&response.data.headers, "To").unwrap()).unwrap());
    ack.data.headers.set(
        Header::new(
            "Via",
            format!(
                "SIP/2.0/TCP 10.0.0.3:5060;branch=z9hG4bK-ack-{}",
                single(&invite.data.headers, "Call-ID").unwrap()
            ),
        )
        .unwrap(),
    );
    ack
}
fn service_local(service: &Service) -> LocalMedia {
    let (ip, port) = service.call.as_ref().unwrap().local_media.unwrap();
    LocalMedia {
        ip,
        port,
        audio_port: service.snapshot.audio_port,
        audio_token: service.snapshot.audio_token.clone(),
    }
}
async fn pure_receive(
    network: &NetworkPair,
    local: &LocalMedia,
    binding: &CallBinding,
    remote: &AudioNegotiation,
) {
    assert_eq!(
        SocketAddr::new(remote.remote_ip, remote.remote_port),
        network.remote.local_addr()
    );
    let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let bridge = SocketAddr::from(([127, 0, 0, 1], local.audio_port));
    let prefix = format!("{}\0", local.audio_token).into_bytes();
    host.send_to(&prefix, bridge).await.unwrap();
    let mut peer = CodecWorker::new(remote.codec).await.unwrap();
    let mut speech = false;
    for index in 0..4 {
        let payload = peer.encode(&voiced(index)).await.unwrap();
        network
            .remote
            .sender()
            .send_to(
                rtp_packet(
                    remote.payload_type,
                    100 + index as u16,
                    32000 + index as u32 * 320,
                    573,
                    &payload,
                ),
                SocketAddr::new(local.ip, local.port),
            )
            .await
            .unwrap();
        let mut pcm = [0u8; 2048];
        let (len, source) = tokio::time::timeout(Duration::from_secs(3), host.recv_from(&mut pcm))
            .await
            .unwrap_or_else(|_| {
                let stats=network.owner.statistics(binding);
                panic!("pure RX frame {index} deadline: codec={:?}, pt={}, carrier={}, destination={}:{}, sent={}, received={}, delivered={}, decode_errors={}, failure={:?}", remote.codec,remote.payload_type,network.remote.local_addr(),local.ip,local.port,stats.rtp_packets_sent,stats.rtp_packets_received,stats.audio_frames_to_host,stats.rtp_decode_errors,stats.failure_code);
            })
            .unwrap();
        assert_eq!(source, bridge);
        assert_eq!(len, prefix.len() + 640);
        assert_eq!(&pcm[..prefix.len()], prefix.as_slice());
        speech |= pcm[prefix.len()..len].iter().any(|byte| *byte != 0);
    }
    assert!(speech);
    let stats = network.owner.statistics(binding);
    assert_eq!(stats.rtp_packets_sent, 0);
    assert_eq!(stats.rtp_packets_received, 4);
    assert_eq!(stats.audio_frames_to_host, 4);
    assert_eq!(stats.rtp_decode_errors, 0);
    peer.close().await.unwrap();
}
fn assert_revoked(snapshot: &Snapshot) {
    assert!(!snapshot.media_ready);
    assert!(!snapshot.early_media);
    assert_eq!(snapshot.audio_port, 0);
    assert!(snapshot.audio_token.is_empty());
}

#[tokio::test]
async fn actual_sip_idle_and_pure_rx_worker_death_revokes_before_join_and_sends_one_bye() {
    // Native owner matrix above covers both exits for both families; these two
    // complete SIP paths additionally cover idle EVS and already-decoding WB.
    for (codec, panic, receive) in [(evs(), false, false), (wb(), true, true)] {
        let plan = Arc::new(FaultPlan::default());
        let _release = ReleaseOnDrop(plan.clone());
        plan.arm(1, None, false, true);
        let mut network = NetworkPair::new(plan.clone()).await;
        let (commands, mut output, writer, release_bye, _release_update, _release_prack) =
            sip_writer(true, false, false);
        let mut service = Service::new(
            network.generation,
            service_config(),
            commands,
            network.owner.clone(),
        )
        .unwrap();
        let observed = Arc::new(Mutex::new(Snapshot::default()));
        let observer = observed.clone();
        service.set_observer(Arc::new(move |_, snapshot| {
            *observer.lock().unwrap() = snapshot
        }));
        let invite = offered_invite(&network, "runtime-failed-active", codec, false);
        service
            .request(SipConnectionId::Primary, invite.clone())
            .await
            .unwrap();
        sip_response(&mut output, 100).await;
        sip_response(&mut output, 180).await;
        service.answer(network.generation).await.unwrap();
        let response = sip_response(&mut output, 200).await;
        let old_ack = offered_ack(&invite, &response);
        service
            .request(SipConnectionId::Primary, old_ack.clone())
            .await
            .unwrap();
        assert_eq!(service.snapshot.state, "active");
        assert!(service.snapshot.media_ready);
        let binding = service.call.as_ref().unwrap().binding.clone();
        let local = service_local(&service);
        let negotiated = service
            .call
            .as_ref()
            .unwrap()
            .remote
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            negotiated.codec, codec,
            "single-codec offer must select the intended real native profile"
        );
        if receive {
            pure_receive(&network, &local, &binding, &negotiated).await;
        }
        let before = network.owner.statistics(&binding);
        let worker = plan.worker(1);
        exit_worker(&worker, panic);
        worker.wait_exit_gate().await;
        stopped(&network.owner, &binding).await;
        let mut ticking = Box::pin(service.tick(network.generation, Instant::now()));
        let bye = tokio::select! {
            wire=sip_wire(&mut output)=>wire,
            result=&mut ticking=>panic!("SIP failure released actual native join too early: {}",result.is_ok()),
        };
        let SipWire::Request(bye) = bye else {
            panic!("active runtime death must send BYE")
        };
        assert_eq!(bye.method, Method::BYE);
        assert_eq!(
            single(&bye.data.headers, "Call-ID").unwrap(),
            "runtime-failed-active"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut ticking)
                .await
                .is_err()
        );
        assert_revoked(&observed.lock().unwrap());
        assert!(network.owner.state.lock().await.is_some());
        assert!(!worker.exited.load(Ordering::SeqCst));
        // The public bridge is already revoked while the BYE transport
        // receipt itself is withheld, before finish can await native stop.
        release_bye.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut ticking)
                .await
                .is_err()
        );
        assert_revoked(&observed.lock().unwrap());
        assert!(
            network
                .owner
                .clone()
                .dtmf(binding.clone(), '#')
                .await
                .is_err()
        );
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut stale = format!("{}\0", local.audio_token).into_bytes();
        stale.extend(voiced(9));
        host.send_to(&stale, SocketAddr::from(([127, 0, 0, 1], local.audio_port)))
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), network.remote.recv())
                .await
                .is_err()
        );
        assert_eq!(
            network.owner.statistics(&binding).rtp_packets_sent,
            before.rtp_packets_sent
        );
        assert!(
            network
                .owner
                .clone()
                .allocate(network.binding("too-early-sip-next-call"))
                .await
                .is_err()
        );
        assert!(
            UdpSocket::bind(("127.0.0.1", local.audio_port))
                .await
                .is_err()
        );
        assert!(matches!(
            output.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        worker.release_exit();
        tokio::time::timeout(Duration::from_secs(3), ticking)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(service.snapshot.state, "failed");
        assert_eq!(service.snapshot.last_error, CODEC_STOPPED);
        assert_revoked(&service.snapshot);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        assert!(!service.snapshot.uncertain);
        assert!(network.owner.state.lock().await.is_none());
        plan.assert_drained();
        service
            .tick(network.generation, Instant::now())
            .await
            .unwrap();
        assert!(
            matches!(output.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "later ticks must not duplicate BYE/final"
        );
        let retry = offered_invite(&network, "runtime-recovered-new-call", codec, false);
        service
            .request(SipConnectionId::Primary, retry.clone())
            .await
            .unwrap();
        sip_response(&mut output, 100).await;
        sip_response(&mut output, 180).await;
        service.answer(network.generation).await.unwrap();
        let response = sip_response(&mut output, 200).await;
        service
            .request(SipConnectionId::Primary, offered_ack(&retry, &response))
            .await
            .unwrap();
        let retry_binding = service.call.as_ref().unwrap().binding.clone();
        let retry_local = service_local(&service);
        assert_eq!(
            service
                .call
                .as_ref()
                .unwrap()
                .remote
                .as_ref()
                .unwrap()
                .codec,
            codec
        );
        assert_ne!(retry_local.audio_token, local.audio_token);
        service
            .request(SipConnectionId::Primary, old_ack)
            .await
            .unwrap();
        assert_eq!(service.snapshot.call_id, "runtime-recovered-new-call");
        assert_eq!(service.snapshot.audio_token, retry_local.audio_token);
        assert!(service.snapshot.media_ready);
        voice(&mut network, &retry_local, &retry_binding, codec).await;
        service.shutdown(network.generation).await.unwrap();
        drop(service);
        tokio::time::timeout(Duration::from_secs(3), writer)
            .await
            .unwrap()
            .unwrap();
        plan.assert_drained();
        network.finish().await;
    }
}

#[tokio::test]
async fn actual_reliable_early_rx_worker_panic_sends_one_final_and_never_bye() {
    let plan = Arc::new(FaultPlan::default());
    let _release = ReleaseOnDrop(plan.clone());
    plan.arm(1, None, false, true);
    let mut network = NetworkPair::new(plan.clone()).await;
    let (commands, mut output, writer, _release_bye, _release_update, _release_prack) =
        sip_writer(false, false, false);
    let mut service = Service::new(
        network.generation,
        service_config(),
        commands,
        network.owner.clone(),
    )
    .unwrap();
    let observed = Arc::new(Mutex::new(Snapshot::default()));
    let observer = observed.clone();
    service.set_observer(Arc::new(move |_, snapshot| {
        *observer.lock().unwrap() = snapshot
    }));
    service
        .request(
            SipConnectionId::Primary,
            offered_invite(&network, "runtime-failed-early", wb(), true),
        )
        .await
        .unwrap();
    sip_response(&mut output, 100).await;
    let ringing = sip_response(&mut output, 180).await;
    assert!(!ringing.data.body.is_empty());
    assert!(token(&ringing.data.headers, "Require", "100rel"));
    assert_eq!(service.snapshot.state, "incoming");
    assert!(service.snapshot.media_ready && service.snapshot.early_media);
    let binding = service.call.as_ref().unwrap().binding.clone();
    let local = service_local(&service);
    let negotiated = service
        .call
        .as_ref()
        .unwrap()
        .remote
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(
        negotiated.codec,
        wb(),
        "reliable early RX must actually use AMR-WB"
    );
    pure_receive(&network, &local, &binding, &negotiated).await;
    let worker = plan.worker(1);
    exit_worker(&worker, true);
    worker.wait_exit_gate().await;
    stopped(&network.owner, &binding).await;
    let mut ticking = Box::pin(service.tick(network.generation, Instant::now()));
    let final_response = tokio::select! {
        wire=sip_wire(&mut output)=>wire,
        result=&mut ticking=>panic!("early SIP failure bypassed native join: {}",result.is_ok()),
    };
    let SipWire::Response(response) = final_response else {
        panic!("early runtime failure must send final, not BYE")
    };
    assert_eq!(response.status, 500);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut ticking)
            .await
            .is_err()
    );
    assert_revoked(&observed.lock().unwrap());
    assert!(network.owner.clone().dtmf(binding, '1').await.is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), network.remote.recv())
            .await
            .is_err()
    );
    worker.release_exit();
    tokio::time::timeout(Duration::from_secs(3), ticking)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(service.snapshot.state, "failed");
    assert_eq!(service.snapshot.last_error, CODEC_STOPPED);
    assert_revoked(&service.snapshot);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    service
        .tick(network.generation, Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        matches!(output.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "early final must stop reliable180 retries and produce no BYE"
    );
    service.shutdown(network.generation).await.unwrap();
    drop(service);
    tokio::time::timeout(Duration::from_secs(3), writer)
        .await
        .unwrap()
        .unwrap();
    plan.assert_drained();
    network.finish().await;
}

#[tokio::test]
async fn confirmed_native_panic_join_does_not_clear_unconfirmed_udp_close_fence() {
    let plan = Arc::new(FaultPlan::default());
    let _release = ReleaseOnDrop(plan.clone());
    plan.arm(1, None, false, true);
    let mut network = NetworkPair::with_retirement(plan.clone(), false).await;
    let binding = network.binding("runtime-failed-udp-close");
    let local = network
        .owner
        .clone()
        .allocate(binding.clone())
        .await
        .unwrap();
    network
        .owner
        .clone()
        .configure(binding.clone(), network.negotiation(evs()))
        .await
        .unwrap();
    network
        .owner
        .clone()
        .start(binding.clone(), false)
        .await
        .unwrap();
    let worker = plan.worker(1);
    exit_worker(&worker, true);
    worker.wait_exit_gate().await;
    stopped(&network.owner, &binding).await;
    // Drop the real registry without its retirement confirmation before native
    // join can finish: the actual PacketSocket.close sees a closed, false watch
    // channel. Ordinary runtime shutdown would acknowledge close correctly.
    network.stop.send_replace(true);
    for task in network.tasks.drain(..) {
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
    }
    worker.release_exit();
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        network.owner.clone().stop(binding.clone()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.uncertain);
    plan.assert_drained();
    assert!(
        network.owner.state.lock().await.is_some(),
        "native join alone cannot acknowledge a failed transport close"
    );
    assert!(
        network
            .owner
            .clone()
            .allocate(network.binding("must-stay-fenced"))
            .await
            .is_err()
    );
    assert!(network.owner.clone().start(binding, false).await.is_err());
    let reclaimed = UdpSocket::bind(("127.0.0.1", local.audio_port))
        .await
        .unwrap();
    drop(reclaimed);
    network.remote.close().await.unwrap();
    // No artificial state clearing: dropping this isolated failed session is
    // the only teardown here; production cannot reuse it for another call.
}

#[tokio::test]
async fn intentional_discard_expiry_and_close_do_not_poison_shared_media_health() {
    let plan = Arc::new(FaultPlan::default());
    let _release = ReleaseOnDrop(plan.clone());
    let mut media = NegotiationState::default();
    media.native_fault = Some(plan.clone());
    let remote = |codec| AudioNegotiation {
        remote_ip: "10.0.0.3".parse().unwrap(),
        remote_port: 4000,
        payload_type: if matches!(codec, AudioCodec::Evs { .. }) {
            96
        } else {
            98
        },
        codec,
        telephone_event_payload: Some(102),
        telephone_event_mask: 0xffff,
    };
    media.configure(remote(evs())).await.unwrap();
    media.prepare(remote(wb())).await.unwrap();
    media.discard().await.unwrap();
    assert!(plan.worker(2).exited.load(Ordering::SeqCst));
    assert!(media.health.failure().is_none());
    assert!(
        !media
            .current
            .as_mut()
            .unwrap()
            .codec
            .encode(&voiced(1))
            .await
            .unwrap()
            .is_empty()
    );
    media.configure(remote(wb())).await.unwrap();
    assert_eq!(media.retired.len(), 1);
    media.retired[0].1 = Instant::now();
    media.expire().await.unwrap();
    assert!(media.retired.is_empty());
    assert!(plan.worker(1).exited.load(Ordering::SeqCst));
    assert!(media.health.failure().is_none());
    assert!(
        !media
            .current
            .as_mut()
            .unwrap()
            .codec
            .encode(&voiced(2))
            .await
            .unwrap()
            .is_empty()
    );
    let health = media.health.clone();
    media.close().await.unwrap();
    assert!(health.failure().is_none());
    plan.assert_drained();
}

#[tokio::test]
async fn retired_worker_with_still_queued_payload_remains_a_fatal_media_dependency() {
    let plan = Arc::new(FaultPlan::default());
    let _release = ReleaseOnDrop(plan.clone());
    plan.arm(1, None, false, true);
    let mut media = NegotiationState::default();
    media.native_fault = Some(plan.clone());
    let remote = |codec| AudioNegotiation {
        remote_ip: "10.0.0.3".parse().unwrap(),
        remote_port: 4000,
        payload_type: if matches!(codec, AudioCodec::Evs { .. }) {
            96
        } else {
            98
        },
        codec,
        telephone_event_payload: Some(102),
        telephone_event_mask: 0xffff,
    };
    media.configure(remote(evs())).await.unwrap();
    let mut peer = CodecWorker::new(evs()).await.unwrap();
    let payload = peer.encode(&voiced(1)).await.unwrap();
    media.configure(remote(wb())).await.unwrap();
    let old = &media.retired[0].0;
    old.ingress.lock().unwrap().push(
        old.id,
        &RtpPacket {
            marker: false,
            payload_type: 96,
            sequence: 100,
            timestamp: 32000,
            ssrc: 573,
            payload: &payload,
        },
        std::time::Instant::now(),
    );
    media.retired[0].1 = Instant::now();
    media.expire().await.unwrap();
    assert_eq!(
        media.retired.len(),
        1,
        "queued old parser must retain its actual native worker"
    );
    let worker = plan.worker(1);
    exit_worker(&worker, false);
    worker.wait_exit_gate().await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), media.health.wait_failure())
            .await
            .unwrap()
            .code,
        CODEC_STOPPED
    );
    let audio = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let stats = Mutex::new(Stats::default());
    assert_eq!(
        receive_ready(&mut media, &audio, b"old-parser\0", None, &stats)
            .await
            .unwrap_err()
            .code,
        CODEC_STOPPED
    );
    assert_eq!(stats.lock().unwrap().rtp_decode_errors, 0);
    let mut closing = Box::pin(media.close());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut closing)
            .await
            .is_err()
    );
    worker.release_exit();
    tokio::time::timeout(Duration::from_secs(3), closing)
        .await
        .unwrap()
        .unwrap();
    peer.close().await.unwrap();
    plan.assert_drained();
}

#[tokio::test]
async fn actual_worker_death_ack_before_tick_revokes_before_bye_receipt_and_join() {
    for (codec, panic) in [(evs(), false), (wb(), true)] {
        let plan = Arc::new(FaultPlan::default());
        let _release = ReleaseOnDrop(plan.clone());
        plan.arm(1, None, false, true);
        let mut network = NetworkPair::new(plan.clone()).await;
        let (commands, mut output, writer, release_bye, _release_update, _release_prack) =
            sip_writer(true, false, false);
        let mut service = Service::new(
            network.generation,
            service_config(),
            commands,
            network.owner.clone(),
        )
        .unwrap();
        let observed = Arc::new(Mutex::new(Snapshot::default()));
        let observer = observed.clone();
        service.set_observer(Arc::new(move |_, snapshot| {
            *observer.lock().unwrap() = snapshot
        }));
        let invite = offered_invite(&network, "runtime-ack-before-tick", codec, false);
        service
            .request(SipConnectionId::Primary, invite.clone())
            .await
            .unwrap();
        sip_response(&mut output, 100).await;
        sip_response(&mut output, 180).await;
        service.answer(network.generation).await.unwrap();
        let response = sip_response(&mut output, 200).await;
        assert_eq!(service.snapshot.state, "connecting");
        assert!(service.snapshot.media_ready && service.snapshot.early_media);
        let binding = service.call.as_ref().unwrap().binding.clone();
        let local = service_local(&service);
        let negotiated = service
            .call
            .as_ref()
            .unwrap()
            .remote
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(negotiated.codec, codec);
        pure_receive(&network, &local, &binding, &negotiated).await;
        let worker = plan.worker(1);
        exit_worker(&worker, panic);
        worker.wait_exit_gate().await;
        stopped(&network.owner, &binding).await;
        // Deliberately do not call tick: this is the first ACK request racing
        // the ordinary 250ms health poll, on a real receive-only call.
        let ack = offered_ack(&invite, &response);
        let mut acknowledging = Box::pin(service.request(SipConnectionId::Primary, ack.clone()));
        let bye = tokio::select! {
            wire=sip_wire(&mut output)=>wire,
            result=&mut acknowledging=>panic!("failed ACK bypassed BYE/join gates: {}",result.is_ok()),
        };
        let SipWire::Request(bye) = bye else {
            panic!("failed media ACK must send exactly one cleanup BYE")
        };
        assert_eq!(bye.method, Method::BYE);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut acknowledging)
                .await
                .is_err()
        );
        assert_revoked(&observed.lock().unwrap());
        assert!(network.owner.state.lock().await.is_some());
        assert!(!worker.exited.load(Ordering::SeqCst));
        assert!(
            network
                .owner
                .clone()
                .dtmf(binding.clone(), '#')
                .await
                .is_err()
        );
        assert!(
            network
                .owner
                .clone()
                .allocate(network.binding("ack-cleanup-too-early"))
                .await
                .is_err()
        );
        assert!(
            UdpSocket::bind(("127.0.0.1", local.audio_port))
                .await
                .is_err()
        );
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut stale = format!("{}\0", local.audio_token).into_bytes();
        stale.extend(voiced(9));
        host.send_to(&stale, SocketAddr::from(([127, 0, 0, 1], local.audio_port)))
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), network.remote.recv())
                .await
                .is_err()
        );
        assert_eq!(network.owner.statistics(&binding).rtp_packets_sent, 0);
        release_bye.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut acknowledging)
                .await
                .is_err()
        );
        assert_revoked(&observed.lock().unwrap());
        worker.release_exit();
        let error = tokio::time::timeout(Duration::from_secs(3), acknowledging)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.code, "ims_media_worker_stopped",
            "preserve the existing request-path control error"
        );
        assert_eq!(service.snapshot.state, "failed");
        assert_eq!(service.snapshot.last_error, error.code);
        assert_revoked(&service.snapshot);
        assert!(!service.call.as_ref().unwrap().media_allocated);
        assert!(network.owner.state.lock().await.is_none());
        plan.assert_drained();
        service
            .request(SipConnectionId::Primary, ack)
            .await
            .unwrap();
        service
            .tick(network.generation, Instant::now())
            .await
            .unwrap();
        assert!(
            matches!(output.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "duplicate ACK/tick must neither restart media nor repeat BYE"
        );
        let retry = offered_invite(&network, "runtime-after-ack-cleanup", codec, false);
        service
            .request(SipConnectionId::Primary, retry.clone())
            .await
            .unwrap();
        sip_response(&mut output, 100).await;
        sip_response(&mut output, 180).await;
        service.answer(network.generation).await.unwrap();
        let response = sip_response(&mut output, 200).await;
        service
            .request(SipConnectionId::Primary, offered_ack(&retry, &response))
            .await
            .unwrap();
        let retry_binding = service.call.as_ref().unwrap().binding.clone();
        let retry_local = service_local(&service);
        assert_ne!(retry_local.audio_token, local.audio_token);
        assert_eq!(
            service
                .call
                .as_ref()
                .unwrap()
                .remote
                .as_ref()
                .unwrap()
                .codec,
            codec
        );
        voice(&mut network, &retry_local, &retry_binding, codec).await;
        service.shutdown(network.generation).await.unwrap();
        drop(service);
        tokio::time::timeout(Duration::from_secs(3), writer)
            .await
            .unwrap()
            .unwrap();
        plan.assert_drained();
        network.finish().await;
    }
}

#[tokio::test]
async fn actual_update_worker_death_after_200_revokes_before_bye_receipt_and_join() {
    let plan = Arc::new(FaultPlan::default());
    let _release = ReleaseOnDrop(plan.clone());
    let mut network = NetworkPair::new(plan.clone()).await;
    let (commands, mut output, writer, release_bye, release_update, _release_prack) =
        sip_writer(true, true, false);
    let mut service = Service::new(
        network.generation,
        service_config(),
        commands,
        network.owner.clone(),
    )
    .unwrap();
    let observed = Arc::new(Mutex::new(Snapshot::default()));
    let observer = observed.clone();
    service.set_observer(Arc::new(move |_, snapshot| {
        *observer.lock().unwrap() = snapshot
    }));
    let invite = offered_invite(&network, "runtime-update-commit", evs(), false);
    service
        .request(SipConnectionId::Primary, invite.clone())
        .await
        .unwrap();
    sip_response(&mut output, 100).await;
    sip_response(&mut output, 180).await;
    service.answer(network.generation).await.unwrap();
    let response = sip_response(&mut output, 200).await;
    service
        .request(SipConnectionId::Primary, offered_ack(&invite, &response))
        .await
        .unwrap();
    let binding = service.call.as_ref().unwrap().binding.clone();
    let local = service_local(&service);
    voice(&mut network, &local, &binding, evs()).await;
    plan.arm(2, None, false, true);
    let mut update = offered_invite(&network, "runtime-update-commit", wb(), false);
    update.method = Method::UPDATE;
    update
        .data
        .headers
        .set(Header::new("CSeq", "2 UPDATE").unwrap());
    update
        .data
        .headers
        .set(Header::new("To", single(&response.data.headers, "To").unwrap()).unwrap());
    update.data.headers.set(
        Header::new(
            "Via",
            "SIP/2.0/TCP 10.0.0.3:5060;branch=z9hG4bK-update-commit",
        )
        .unwrap(),
    );
    let mut updating = Box::pin(service.request(SipConnectionId::Primary, update));
    let accepted = tokio::select! {
        wire=sip_wire(&mut output)=>wire,
        result=&mut updating=>panic!("UPDATE must await actual200 receipt before commit: {}",result.is_ok()),
    };
    let SipWire::Response(accepted) = accepted else {
        panic!("UPDATE must produce its SDP answer before cleanup")
    };
    assert_eq!(accepted.status, 200);
    assert_eq!(cseq(&accepted.data.headers).unwrap(), (2, Method::UPDATE));
    assert!(
        std::str::from_utf8(&accepted.data.body)
            .unwrap()
            .contains("AMR-WB/16000")
    );
    let worker = plan.worker(2);
    assert_eq!(worker.profile, CodecProfile::AmrWb { mode: 8 });
    exit_worker(&worker, true);
    worker.wait_exit_gate().await;
    stopped(&network.owner, &binding).await;
    // The accepted200 receipt unlocks the production commit_media call. The
    // actual staged WB thread died meanwhile; no fake configure error is used.
    release_update.send(()).unwrap();
    let bye = tokio::select! {
        wire=sip_wire(&mut output)=>wire,
        result=&mut updating=>panic!("accepted UPDATE failure bypassed BYE/join gates: {}",result.is_ok()),
    };
    let SipWire::Request(bye) = bye else {
        panic!("failed accepted UPDATE must send cleanup BYE")
    };
    assert_eq!(bye.method, Method::BYE);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut updating)
            .await
            .is_err()
    );
    assert_revoked(&observed.lock().unwrap());
    assert!(network.owner.state.lock().await.is_some());
    assert!(
        network
            .owner
            .clone()
            .dtmf(binding.clone(), '#')
            .await
            .is_err()
    );
    assert!(
        network
            .owner
            .clone()
            .allocate(network.binding("update-cleanup-too-early"))
            .await
            .is_err()
    );
    assert_eq!(network.owner.statistics(&binding).rtp_packets_sent, 4);
    release_bye.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut updating)
            .await
            .is_err()
    );
    assert_revoked(&observed.lock().unwrap());
    worker.release_exit();
    let error = tokio::time::timeout(Duration::from_secs(3), updating)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "ims_media_worker_stopped");
    assert_eq!(service.snapshot.last_error, error.code);
    assert_eq!(service.snapshot.state, "failed");
    assert_revoked(&service.snapshot);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    assert!(network.owner.state.lock().await.is_none());
    plan.assert_drained();
    service
        .tick(network.generation, Instant::now())
        .await
        .unwrap();
    assert!(
        matches!(output.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "UPDATE failure cannot emit another final/ACK/BYE or restore readiness"
    );
    service.shutdown(network.generation).await.unwrap();
    drop(service);
    tokio::time::timeout(Duration::from_secs(3), writer)
        .await
        .unwrap()
        .unwrap();
    plan.assert_drained();
    network.finish().await;
}

#[tokio::test]
async fn actual_prack_native_prepare_failure_revokes_before_prack200_and_drains_before_reuse() {
    let plan = Arc::new(FaultPlan::default());
    let _release = ReleaseOnDrop(plan.clone());
    // Only the second constructor fails: real WB calloc2, on its actual codec
    // thread. The first, still-serving EVS context is held at its final exit.
    plan.arm(2, Some(2), false, false);
    plan.state.lock().unwrap().pause_exit_worker = Some(1);
    let mut network = NetworkPair::new(plan.clone()).await;
    let (commands, mut output, writer, _release_bye, _release_update, release_prack) =
        sip_writer(false, false, true);
    let mut service = Service::new(
        network.generation,
        service_config(),
        commands,
        network.owner.clone(),
    )
    .unwrap();
    let observed = Arc::new(Mutex::new(Snapshot::default()));
    let observer = observed.clone();
    service.set_observer(Arc::new(move |_, snapshot| {
        *observer.lock().unwrap() = snapshot
    }));
    let invite = offered_invite(&network, "runtime-prack-prepare-failure", evs(), true);
    service
        .request(SipConnectionId::Primary, invite.clone())
        .await
        .unwrap();
    sip_response(&mut output, 100).await;
    let ringing = sip_response(&mut output, 180).await;
    assert!(token(&ringing.data.headers, "Require", "100rel"));
    assert_eq!(service.snapshot.state, "incoming");
    assert!(service.snapshot.media_ready && service.snapshot.early_media);
    let binding = service.call.as_ref().unwrap().binding.clone();
    let local = service_local(&service);
    let negotiated = service
        .call
        .as_ref()
        .unwrap()
        .remote
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(negotiated.codec, evs());
    pure_receive(&network, &local, &binding, &negotiated).await;
    let mut prack = offered_invite(&network, "runtime-prack-prepare-failure", wb(), false);
    prack.method = Method::PRACK;
    prack
        .data
        .headers
        .set(Header::new("CSeq", "2 PRACK").unwrap());
    prack
        .data
        .headers
        .set(Header::new("To", single(&ringing.data.headers, "To").unwrap()).unwrap());
    prack.data.headers.set(
        Header::new(
            "Via",
            "SIP/2.0/TCP 10.0.0.3:5060;branch=z9hG4bK-prack-native-failure",
        )
        .unwrap(),
    );
    push(
        &mut prack.data.headers,
        "RAck",
        &format!(
            "{} 1 INVITE",
            single(&ringing.data.headers, "RSeq").unwrap()
        ),
    )
    .unwrap();
    let mut pracking = Box::pin(service.request(SipConnectionId::Primary, prack.clone()));
    let acknowledged = tokio::select! {
        wire=sip_wire(&mut output)=>wire,
        result=&mut pracking=>panic!("PRACK failure bypassed its200 receipt: {}",result.is_ok()),
    };
    let SipWire::Response(acknowledged) = acknowledged else {
        panic!("PRACK must receive200 before initial INVITE final, never BYE")
    };
    assert_eq!(acknowledged.status, 200);
    assert_eq!(
        cseq(&acknowledged.data.headers).unwrap(),
        (2, Method::PRACK)
    );
    assert!(
        std::str::from_utf8(&acknowledged.data.body)
            .unwrap()
            .contains("m=audio 0 "),
        "a failed new offer must receive an honest rejected stream"
    );
    let prack_bytes = proletarius_sip::Message::Response(acknowledged).encode();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut pracking)
            .await
            .is_err()
    );
    assert_revoked(&observed.lock().unwrap());
    let failed = plan.worker(2);
    assert_eq!(failed.profile, CodecProfile::AmrWb { mode: 8 });
    assert!(failed.exited.load(Ordering::SeqCst));
    let facts = report(failed.token.load(Ordering::SeqCst)).unwrap();
    assert_eq!(
        (facts.injected_failures, facts.live_blocks, facts.live_bytes),
        (1, 0, 0)
    );
    let original = plan.worker(1);
    assert!(!original.exit_entered.load(Ordering::SeqCst));
    assert!(
        report(original.token.load(Ordering::SeqCst))
            .unwrap()
            .live_blocks
            > 0,
        "the old real EVS worker is still alive while PRACK200 receipt is held"
    );
    assert!(network.owner.state.lock().await.is_some());
    assert!(
        UdpSocket::bind(("127.0.0.1", local.audio_port))
            .await
            .is_err()
    );
    assert!(
        network
            .owner
            .clone()
            .allocate(network.binding("prack-cleanup-too-early"))
            .await
            .is_err()
    );
    assert!(
        network
            .owner
            .clone()
            .dtmf(binding.clone(), '#')
            .await
            .is_err()
    );
    let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut stale = format!("{}\0", local.audio_token).into_bytes();
    stale.extend(voiced(9));
    host.send_to(&stale, SocketAddr::from(([127, 0, 0, 1], local.audio_port)))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), network.remote.recv())
            .await
            .is_err()
    );
    let stats = network.owner.statistics(&binding);
    assert_eq!(
        (
            stats.rtp_packets_sent,
            stats.rtp_packets_received,
            stats.audio_frames_to_host,
            stats.rtp_decode_errors
        ),
        (0, 4, 4, 0)
    );
    assert!(
        matches!(output.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "no INVITE final may precede the PRACK200 receipt"
    );
    release_prack.send(()).unwrap();
    let final_response = tokio::select! {
        wire=sip_wire(&mut output)=>wire,
        result=&mut pracking=>panic!("PRACK failed media must await old EVS join: {}",result.is_ok()),
    };
    let SipWire::Response(final_response) = final_response else {
        panic!("early failure must send initial500, never BYE")
    };
    assert_eq!(final_response.status, 500);
    assert_eq!(
        cseq(&final_response.data.headers).unwrap(),
        (1, Method::INVITE)
    );
    assert_eq!(
        single(&final_response.data.headers, "Call-ID").unwrap(),
        "runtime-prack-prepare-failure"
    );
    assert!(final_response.data.body.is_empty());
    let final_bytes = proletarius_sip::Message::Response(final_response).encode();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut pracking)
            .await
            .is_err()
    );
    original.wait_exit_gate().await;
    assert_revoked(&observed.lock().unwrap());
    assert!(!original.exited.load(Ordering::SeqCst));
    assert!(
        UdpSocket::bind(("127.0.0.1", local.audio_port))
            .await
            .is_err()
    );
    assert!(matches!(
        output.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    original.release_exit();
    let error = tokio::time::timeout(Duration::from_secs(3), pracking)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "ims_media_codec_unavailable");
    assert_eq!(service.snapshot.last_error, error.code);
    assert_eq!(service.snapshot.state, "failed");
    assert_revoked(&service.snapshot);
    assert!(!service.call.as_ref().unwrap().media_allocated);
    assert!(network.owner.state.lock().await.is_none());
    plan.assert_drained();
    service
        .tick(network.generation, Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        matches!(output.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "no reliable180/final/BYE restart after confirmed cleanup"
    );
    service
        .request(SipConnectionId::Primary, prack.clone())
        .await
        .unwrap();
    assert_eq!(
        proletarius_sip::Message::Response(sip_response(&mut output, 200).await).encode(),
        prack_bytes
    );
    service
        .request(SipConnectionId::Primary, invite)
        .await
        .unwrap();
    assert_eq!(
        proletarius_sip::Message::Response(sip_response(&mut output, 500).await).encode(),
        final_bytes
    );
    // Exact old PRACK replay after a new active call must neither revoke the
    // new bridge nor reacquire failed/retired media.
    let retry = offered_invite(&network, "runtime-after-prack-cleanup", evs(), false);
    service
        .request(SipConnectionId::Primary, retry.clone())
        .await
        .unwrap();
    sip_response(&mut output, 100).await;
    sip_response(&mut output, 180).await;
    service.answer(network.generation).await.unwrap();
    let response = sip_response(&mut output, 200).await;
    service
        .request(SipConnectionId::Primary, offered_ack(&retry, &response))
        .await
        .unwrap();
    let retry_binding = service.call.as_ref().unwrap().binding.clone();
    let retry_local = service_local(&service);
    assert_ne!(retry_local.audio_token, local.audio_token);
    service
        .request(SipConnectionId::Primary, prack)
        .await
        .unwrap();
    assert_eq!(
        proletarius_sip::Message::Response(sip_response(&mut output, 200).await).encode(),
        prack_bytes
    );
    assert_eq!(service.snapshot.state, "active");
    assert!(service.snapshot.media_ready);
    assert_eq!(service.snapshot.audio_token, retry_local.audio_token);
    voice(&mut network, &retry_local, &retry_binding, evs()).await;
    service.shutdown(network.generation).await.unwrap();
    drop(service);
    tokio::time::timeout(Duration::from_secs(3), writer)
        .await
        .unwrap()
        .unwrap();
    plan.assert_drained();
    network.finish().await;
}
