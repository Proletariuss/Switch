use super::*;
use proletarius_vowifi::{
    TunnelConfiguration, netstack_runtime::TunnelNetwork, netstack_udp::UdpSocketRegistry,
};
use smoltcp::iface::SocketSet;
#[tokio::test]
async fn actual_native_rtp_pcm_bridge_authenticates_holds_transmit_and_drains_workers() {
    let config = |ip: &str| TunnelConfiguration {
        ipv4_addresses: vec![ip.parse().unwrap()],
        ..Default::default()
    };
    let a = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
    let b = TunnelNetwork::new(&config("10.0.0.3")).unwrap();
    let (ab_tx, ab_rx) = mpsc::channel(256);
    let (ba_tx, ba_rx) = mpsc::channel(256);
    let (stop, stop_rx) = watch::channel(false);
    let (wa, ra) = watch::channel(0);
    let (wb, rb) = watch::channel(0);
    let (ca, sa) = UdpSocketRegistry::new(wa);
    let (cb, sb) = UdpSocketRegistry::new(wb);
    let client = tokio::spawn(a.run_managed(
        SocketSet::new(vec![]),
        ba_rx,
        ab_tx,
        stop_rx.clone(),
        ra,
        sa,
    ));
    let server = tokio::spawn(b.run_managed(SocketSet::new(vec![]), ab_rx, ba_tx, stop_rx, rb, sb));
    let generation = SessionGeneration::from_value(11).unwrap();
    let binding = CallBinding {
        generation,
        call_id: "actual-media".into(),
        invite_cseq: 1,
    };
    let owner = Owner::new(generation, "10.0.0.2".parse().unwrap(), ca);
    let local = owner.clone().allocate(binding.clone()).await.unwrap();
    let mut remote = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
    let (evs, _) = proletarius_call::evs::available_audio_codecs(false);
    assert!(evs, "the actual EVS library is required");
    let codec = AudioCodec::Evs { header_full: false };
    {
        owner
            .clone()
            .configure(
                binding.clone(),
                AudioNegotiation {
                    remote_ip: remote.local_addr().ip(),
                    remote_port: remote.local_addr().port(),
                    payload_type: 96,
                    codec,
                    telephone_event_payload: Some(102),
                    telephone_event_mask: 0xffff,
                },
            )
            .await
            .unwrap();
        owner.clone().start(binding.clone(), false).await.unwrap();
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bridge = SocketAddr::new("127.0.0.1".parse().unwrap(), local.audio_port);
        let mut bad = b"wrong-token\\0".to_vec();
        bad.extend_from_slice(&[0; 640]);
        host.send_to(&bad, bridge).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), remote.recv())
                .await
                .is_err()
        );
        let prefix = format!("{}\0", local.audio_token).into_bytes();
        host.send_to(&prefix, bridge).await.unwrap();
        let mut pcm = prefix.clone();
        pcm.extend_from_slice(&[0; 640]);
        host.send_to(&pcm, bridge).await.unwrap();
        let encoded = tokio::time::timeout(Duration::from_secs(2), remote.recv())
            .await
            .unwrap()
            .unwrap();
        let parsed = RtpPacket::parse(&encoded.bytes).unwrap();
        assert_eq!(parsed.payload_type, 96);
        assert!(!parsed.payload.is_empty());
        remote
            .sender()
            .send_to(
                encoded.bytes.to_vec(),
                SocketAddr::new(local.ip, local.port),
            )
            .await
            .unwrap();
        let mut decoded = [0; 2048];
        let (count, _) = tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut decoded))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, prefix.len() + 640);
        assert_eq!(&decoded[..prefix.len()], prefix.as_slice());
        let first_header = (parsed.ssrc, parsed.sequence, parsed.timestamp);
        owner
            .clone()
            .hold_transmit(binding.clone(), true)
            .await
            .unwrap();
        owner
            .clone()
            .hold_transmit(binding.clone(), true)
            .await
            .unwrap();
        host.send_to(&pcm, bridge).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), remote.recv())
                .await
                .is_err(),
            "authenticated PCM emits no RTP while transmit is held"
        );
        assert_eq!(
            owner
                .clone()
                .dtmf(binding.clone(), '#')
                .await
                .unwrap_err()
                .code,
            "ims_media_negotiating"
        );
        let stale_hold = CallBinding {
            call_id: "retired-hold-call".into(),
            ..binding.clone()
        };
        assert!(
            owner
                .clone()
                .hold_transmit(stale_hold, false)
                .await
                .is_err()
        );
        let mut voice = CodecWorker::new(codec).await.unwrap();
        let incoming_voice = voice.encode(&voiced_pcm(0)).await.unwrap();
        remote
            .sender()
            .send_to(
                rtp_packet(
                    96,
                    first_header.1.wrapping_add(1),
                    first_header.2.wrapping_add(320),
                    first_header.0,
                    &incoming_voice,
                ),
                SocketAddr::new(local.ip, local.port),
            )
            .await
            .unwrap();
        let (count, _) = tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut decoded))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            count,
            prefix.len() + 640,
            "receive still decodes while transmit is held"
        );
        assert_eq!(&decoded[..prefix.len()], prefix.as_slice());
        voice.close().await.unwrap();
        owner
            .clone()
            .hold_transmit(binding.clone(), false)
            .await
            .unwrap();
        owner
            .clone()
            .hold_transmit(binding.clone(), false)
            .await
            .unwrap();
        host.send_to(&pcm, bridge).await.unwrap();
        let resumed = tokio::time::timeout(Duration::from_secs(2), remote.recv())
            .await
            .unwrap()
            .unwrap();
        let resumed = RtpPacket::parse(&resumed.bytes).unwrap();
        assert_eq!(
            (resumed.ssrc, resumed.sequence, resumed.timestamp),
            (
                first_header.0,
                first_header.1.wrapping_add(1),
                first_header.2.wrapping_add(320)
            ),
            "hold does not encode discarded PCM or advance RTP counters"
        );
        owner.clone().dtmf(binding.clone(), '#').await.unwrap();
        let mut events = vec![];
        for _ in 0..7 {
            let packet = remote.recv().await.unwrap();
            let parsed = RtpPacket::parse(&packet.bytes).unwrap();
            assert_eq!(parsed.payload_type, 102);
            events.push(parsed.payload.to_vec());
        }
        assert_eq!(events[0][0], 11);
        assert_eq!(events[6][1] & 0x80, 0x80);
        let mut updated_peer = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
        owner
            .clone()
            .configure(
                binding.clone(),
                AudioNegotiation {
                    remote_ip: updated_peer.local_addr().ip(),
                    remote_port: updated_peer.local_addr().port(),
                    payload_type: 97,
                    codec,
                    telephone_event_payload: Some(102),
                    telephone_event_mask: 0xffff,
                },
            )
            .await
            .unwrap();
        host.send_to(&pcm, bridge).await.unwrap();
        let updated = tokio::time::timeout(Duration::from_secs(2), updated_peer.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(RtpPacket::parse(&updated.bytes).unwrap().payload_type, 97);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), remote.recv())
                .await
                .is_err()
        );
        updated_peer
            .sender()
            .send_to(
                updated.bytes.to_vec(),
                SocketAddr::new(local.ip, local.port),
            )
            .await
            .unwrap();
        let (count, _) = tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut decoded))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, prefix.len() + 640);
        let stats = owner.statistics(&binding);
        assert_eq!(stats.rtp_packets_sent, 10);
        assert_eq!(stats.rtp_packets_received, 3);
        assert_eq!(stats.audio_frames_to_host, 3);
        owner
            .clone()
            .configure(
                binding.clone(),
                AudioNegotiation {
                    remote_ip: updated_peer.local_addr().ip(),
                    remote_port: updated_peer.local_addr().port(),
                    payload_type: 97,
                    codec,
                    telephone_event_payload: Some(102),
                    telephone_event_mask: 0x03ff,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            owner
                .clone()
                .dtmf(binding.clone(), '#')
                .await
                .unwrap_err()
                .code,
            "ims_media_dtmf_unavailable"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), updated_peer.recv())
                .await
                .is_err(),
            "a digit outside the negotiated telephone-event subset emits no RTP"
        );
        owner.clone().dtmf(binding.clone(), '3').await.unwrap();
        for _ in 0..7 {
            let packet = updated_peer.recv().await.unwrap();
            let parsed = RtpPacket::parse(&packet.bytes).unwrap();
            assert_eq!(parsed.payload_type, 102);
            assert_eq!(parsed.payload[0], 3);
        }
        updated_peer.close().await.unwrap();
    }
    let stale = CallBinding {
        generation: SessionGeneration::from_value(12).unwrap(),
        ..binding.clone()
    };
    assert!(owner.clone().stop(stale).await.is_err());
    owner.clone().stop(binding.clone()).await.unwrap();
    assert!(owner.clone().start(binding, false).await.is_err());
    remote.close().await.unwrap();
    stop.send_replace(true);
    client.await.unwrap().unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn network_failure_first_proves_exact_socket_retirement_before_media_stop() {
    let config = TunnelConfiguration {
        ipv4_addresses: vec!["10.0.0.2".parse().unwrap()],
        ..Default::default()
    };
    let network = TunnelNetwork::new(&config).unwrap();
    let (wake, rx) = watch::channel(0);
    let (udp, registry) = UdpSocketRegistry::new(wake);
    let (_inbound, input) = mpsc::channel(32);
    let (outbound, output) = mpsc::channel(32);
    let (_stop, stop) = watch::channel(false);
    let task = tokio::spawn(network.run_managed(
        SocketSet::new(vec![]),
        input,
        outbound,
        stop,
        rx,
        registry,
    ));
    let generation = SessionGeneration::from_value(13).unwrap();
    let binding = CallBinding {
        generation,
        call_id: "network-loss".into(),
        invite_cseq: 1,
    };
    let owner = Owner::new(generation, "10.0.0.2".parse().unwrap(), udp);
    owner.clone().allocate(binding.clone()).await.unwrap();
    let codec = AudioCodec::Evs { header_full: false };
    {
        owner
            .clone()
            .configure(
                binding.clone(),
                AudioNegotiation {
                    remote_ip: "10.0.0.3".parse().unwrap(),
                    remote_port: 49170,
                    payload_type: 96,
                    codec,
                    telephone_event_payload: Some(102),
                    telephone_event_mask: 0xffff,
                },
            )
            .await
            .unwrap();
        owner.clone().start(binding.clone(), false).await.unwrap();
    }
    drop(output);
    assert!(task.await.unwrap().is_err());
    tokio::time::timeout(Duration::from_secs(2), owner.clone().stop(binding.clone()))
        .await
        .unwrap()
        .unwrap();
    assert!(
        owner.state.lock().await.is_none(),
        "actual network removal and codec join release the media owner"
    );
}

#[tokio::test]
async fn amr_wb_cmr_changes_one_worker_on_frame_boundaries_in_both_formats() {
    use proletarius_call::sdp::AmrWbModePolicy;
    for octet_aligned in [false, true] {
        let policy = AmrWbModePolicy::new((1 << 2) | (1 << 5) | (1 << 8), 2, true).unwrap();
        let mut worker = CodecWorker::new(AudioCodec::AmrWb {
            octet_aligned,
            mode: 8,
            policy,
        })
        .await
        .unwrap();
        let pcm = [0; 640];
        let initial = worker.encode(&pcm).await.unwrap();
        assert_eq!(
            amr_wb::depacketize(&initial, octet_aligned).unwrap().mode,
            8
        );
        let mut request = initial.clone();
        request[0] = (request[0] & 0x0f) | (2 << 4);
        assert_eq!(worker.decode(&request).await.unwrap().len(), 640);
        // Frame 0 was already encoded; period=2 keeps frame 1 at mode8,
        // then steps through the neighboring member of the active set.
        for expected in [8, 5, 5, 2, 2] {
            let payload = worker.encode(&pcm).await.unwrap();
            assert_eq!(
                amr_wb::depacketize(&payload, octet_aligned).unwrap().mode,
                expected
            );
        }
        for ignored in [0, 1, 3, 4, 6, 7, 9, 10, 11, 12, 13, 14, 15] {
            request[0] = (request[0] & 0x0f) | (ignored << 4);
            worker.decode(&request).await.unwrap();
            let payload = worker.encode(&pcm).await.unwrap();
            assert_eq!(
                amr_wb::depacketize(&payload, octet_aligned).unwrap().mode,
                2
            );
        }
        // Bad speech quality still has a structurally valid CMR header.
        // Conceal the frame without passing its nonempty bits as lost audio.
        let mut bad_quality = request.clone();
        bad_quality[0] = (bad_quality[0] & 0x0f) | (9 << 4);
        bad_quality[1] &= if octet_aligned { !0x04 } else { !0x40 };
        assert_eq!(worker.decode(&bad_quality).await.unwrap().len(), 640);
        // A malformed packet may not change CMR policy, even if its header
        // asks for a supported mode. The decoder rejects before adaptation.
        request[0] = (request[0] & 0x0f) | (8 << 4);
        request.pop();
        assert!(worker.decode(&request).await.is_err());
        assert_eq!(worker.transmit_mode, 2);
        assert_eq!(worker.requested_mode, None);
        worker.close().await.unwrap();
    }
}

#[tokio::test]
async fn amr_wb_no_request_cancels_pending_neighbor_steps_without_resetting_rate() {
    use proletarius_call::sdp::AmrWbModePolicy;
    let policy = AmrWbModePolicy::new((1 << 2) | (1 << 5) | (1 << 8), 1, true).unwrap();
    let mut worker = CodecWorker::new(AudioCodec::AmrWb {
        octet_aligned: true,
        mode: 8,
        policy,
    })
    .await
    .unwrap();
    let pcm = [0; 640];
    let mut request = worker.encode(&pcm).await.unwrap();
    request[0] = 2 << 4;
    worker.decode(&request).await.unwrap();
    assert_eq!(
        amr_wb::depacketize(&worker.encode(&pcm).await.unwrap(), true)
            .unwrap()
            .mode,
        5
    );
    request[0] = 0xf0;
    worker.decode(&request).await.unwrap();
    for _ in 0..3 {
        assert_eq!(
            amr_wb::depacketize(&worker.encode(&pcm).await.unwrap(), true)
                .unwrap()
                .mode,
            5
        );
    }
    // Out-of-set incoming speech is rejected along with its request.
    let mut other = EvsCodec::new(CodecProfile::AmrWb { mode: 0 }).unwrap();
    let bits = other.encode(&[0; 320]).unwrap();
    let mut outside = amr_wb::packetize(&bits, 0, true).unwrap();
    outside[0] = 8 << 4;
    assert!(worker.decode(&outside).await.is_err());
    assert_eq!(worker.requested_mode, None);
    worker.close().await.unwrap();
}

#[tokio::test]
async fn amr_wb_reserved_cmr_does_not_cancel_a_pending_neighbor_transition() {
    use proletarius_call::sdp::AmrWbModePolicy;
    for ignored in [0, 9, 14] {
        let policy = AmrWbModePolicy::new((1 << 2) | (1 << 5) | (1 << 8), 1, true).unwrap();
        let mut worker = CodecWorker::new(AudioCodec::AmrWb {
            octet_aligned: true,
            mode: 8,
            policy,
        })
        .await
        .unwrap();
        let mut request = worker.encode(&[0; 640]).await.unwrap();
        request[0] = 2 << 4;
        worker.decode(&request).await.unwrap();
        assert_eq!(
            amr_wb::depacketize(&worker.encode(&[0; 640]).await.unwrap(), true)
                .unwrap()
                .mode,
            5
        );
        request[0] = ignored << 4;
        worker.decode(&request).await.unwrap();
        assert_eq!(
            amr_wb::depacketize(&worker.encode(&[0; 640]).await.unwrap(), true)
                .unwrap()
                .mode,
            2
        );
        worker.close().await.unwrap();
    }
}

#[tokio::test]
async fn amr_wb_cmr_respects_rtp_source_order_and_call_binding() {
    use proletarius_call::sdp::AmrWbModePolicy;
    for octet_aligned in [false, true] {
        let config = |ip: &str| TunnelConfiguration {
            ipv4_addresses: vec![ip.parse().unwrap()],
            ..Default::default()
        };
        let a = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
        let b = TunnelNetwork::new(&config("10.0.0.3")).unwrap();
        let (ab_tx, ab_rx) = mpsc::channel(256);
        let (ba_tx, ba_rx) = mpsc::channel(256);
        let (stop, stop_rx) = watch::channel(false);
        let (wa, ra) = watch::channel(0);
        let (wb, rb) = watch::channel(0);
        let (ca, sa) = UdpSocketRegistry::new(wa);
        let (cb, sb) = UdpSocketRegistry::new(wb);
        let client = tokio::spawn(a.run_managed(
            SocketSet::new(vec![]),
            ba_rx,
            ab_tx,
            stop_rx.clone(),
            ra,
            sa,
        ));
        let server =
            tokio::spawn(b.run_managed(SocketSet::new(vec![]), ab_rx, ba_tx, stop_rx, rb, sb));
        let generation = SessionGeneration::from_value(600).unwrap();
        let binding = CallBinding {
            generation,
            call_id: "cmr-live-order".into(),
            invite_cseq: 1,
        };
        let owner = Owner::new(generation, "10.0.0.2".parse().unwrap(), ca);
        let local = owner.clone().allocate(binding.clone()).await.unwrap();
        let mut remote = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
        let mut wrong_source = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
        let codec = AudioCodec::AmrWb {
            octet_aligned,
            mode: 8,
            policy: AmrWbModePolicy::new((1 << 2) | (1 << 8), 1, false).unwrap(),
        };
        let negotiation = AudioNegotiation {
            remote_ip: remote.local_addr().ip(),
            remote_port: remote.local_addr().port(),
            payload_type: 98,
            codec,
            telephone_event_payload: None,
            telephone_event_mask: 0,
        };
        owner
            .clone()
            .configure(binding.clone(), negotiation.clone())
            .await
            .unwrap();
        owner.clone().start(binding.clone(), false).await.unwrap();
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bridge = SocketAddr::new("127.0.0.1".parse().unwrap(), local.audio_port);
        let prefix = format!("{}\0", local.audio_token).into_bytes();
        let mut pcm = prefix.clone();
        pcm.extend_from_slice(&[0; 640]);
        host.send_to(&pcm, bridge).await.unwrap();
        let first = tokio::time::timeout(Duration::from_secs(2), remote.recv())
            .await
            .unwrap()
            .unwrap();
        let first = RtpPacket::parse(&first.bytes).unwrap();
        assert_eq!(
            amr_wb::depacketize(first.payload, octet_aligned)
                .unwrap()
                .mode,
            8
        );
        let incoming = first.payload.to_vec();
        let mut previous = (first.ssrc, first.sequence, first.timestamp);
        let destination = SocketAddr::new(local.ip, local.port);
        let mut decoded = [0; 2048];
        for (sequence, cmr) in [(100, 2), (101, 9), (102, 14)] {
            let mut request = incoming.clone();
            request[0] = (request[0] & 0x0f) | (cmr << 4);
            if sequence == 101 {
                let mut stale = incoming.clone();
                stale[0] = (stale[0] & 0x0f) | (8 << 4);
                for old_sequence in [100, 99] {
                    remote
                        .sender()
                        .send_to(
                            rtp_packet(98, old_sequence, 32000, 777, &stale),
                            destination,
                        )
                        .await
                        .unwrap();
                }
            }
            if sequence == 102 {
                let mut wrong = incoming.clone();
                wrong[0] = (wrong[0] & 0x0f) | (8 << 4);
                wrong_source
                    .sender()
                    .send_to(rtp_packet(98, 900, 32000, 888, &wrong), destination)
                    .await
                    .unwrap();
                let stale_binding = CallBinding {
                    call_id: "retired-call".into(),
                    ..binding.clone()
                };
                assert!(
                    owner
                        .clone()
                        .configure(stale_binding.clone(), negotiation.clone())
                        .await
                        .is_err()
                );
                assert!(owner.clone().stop(stale_binding).await.is_err());
            }
            remote
                .sender()
                .send_to(
                    rtp_packet(98, sequence, u32::from(sequence) * 320, 777, &request),
                    destination,
                )
                .await
                .unwrap();
            let (count, _) =
                tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut decoded))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(count, prefix.len() + 640); // ordered CMR decode barrier
            host.send_to(&pcm, bridge).await.unwrap();
            let next = tokio::time::timeout(Duration::from_secs(2), remote.recv())
                .await
                .unwrap()
                .unwrap();
            let next = RtpPacket::parse(&next.bytes).unwrap();
            assert_eq!(
                amr_wb::depacketize(next.payload, octet_aligned)
                    .unwrap()
                    .mode,
                2
            );
            assert_eq!(
                (next.ssrc, next.sequence, next.timestamp),
                (
                    previous.0,
                    previous.1.wrapping_add(1),
                    previous.2.wrapping_add(320)
                )
            );
            previous = (next.ssrc, next.sequence, next.timestamp);
        }
        assert_eq!(owner.statistics(&binding).rtp_packets_received, 3);
        assert_eq!(owner.statistics(&binding).rtp_decode_errors, 0);
        owner.clone().stop(binding.clone()).await.unwrap();
        assert!(owner.clone().start(binding, false).await.is_err());
        remote.close().await.unwrap();
        wrong_source.close().await.unwrap();
        stop.send_replace(true);
        client.await.unwrap().unwrap();
        server.await.unwrap().unwrap();
    }
}

fn voiced_pcm(frame: usize) -> Vec<u8> {
    (0..320)
        .flat_map(|sample| {
            let t = ((frame * 320 + sample) as f64) / 16000.0;
            let value = (7000.0 * (std::f64::consts::TAU * 220.0 * t).sin()
                + 2100.0 * (std::f64::consts::TAU * 440.0 * t).sin())
                as i16;
            value.to_le_bytes()
        })
        .collect()
}

#[tokio::test]
async fn staged_policy_preserves_native_history_phase_and_excludes_old_mode_immediately() {
    use proletarius_call::sdp::AmrWbModePolicy;
    let original = AudioNegotiation {
        remote_ip: "10.0.0.3".parse().unwrap(),
        remote_port: 4000,
        payload_type: 98,
        telephone_event_payload: None,
        telephone_event_mask: 0,
        codec: AudioCodec::AmrWb {
            octet_aligned: true,
            mode: 8,
            policy: AmrWbModePolicy::new((1 << 2) | (1 << 5) | (1 << 8), 2, true).unwrap(),
        },
    };
    let mut media = NegotiationState::default();
    assert!(media.configure(original.clone()).await.unwrap());
    let pcm = voiced_pcm(0);
    let current = media.current.as_mut().unwrap();
    let mut request = current.codec.encode(&pcm).await.unwrap();
    request[0] = (request[0] & 0xf) | (2 << 4);
    current.codec.decode(&request).await.unwrap();
    let native = current.codec.native.as_ref().unwrap().clone();
    assert_eq!(current.codec.encoded_frames, 1);
    let mut candidate = original.clone();
    candidate.codec = AudioCodec::AmrWb {
        octet_aligned: true,
        mode: 5,
        policy: AmrWbModePolicy::new((1 << 0) | (1 << 5), 2, true).unwrap(),
    };
    media.prepare(candidate.clone()).await.unwrap();
    media.prepare(candidate.clone()).await.unwrap();
    assert!(Arc::ptr_eq(
        &native,
        media
            .pending
            .as_ref()
            .unwrap()
            .codec
            .native
            .as_ref()
            .unwrap()
    ));
    // Old negotiation keeps encoding while the replacement is only prepared.
    assert_eq!(
        amr_wb::depacketize(
            &media
                .current
                .as_mut()
                .unwrap()
                .codec
                .encode(&pcm)
                .await
                .unwrap(),
            true
        )
        .unwrap()
        .mode,
        8
    );
    assert!(media.configure(candidate.clone()).await.unwrap());
    let current = media.current.as_mut().unwrap();
    assert!(Arc::ptr_eq(&native, current.codec.native.as_ref().unwrap()));
    assert_eq!(current.codec.encoded_frames, 2);
    assert_eq!(current.codec.requested_mode, None); // old CMR2 is outside {0,5}
    assert_eq!(
        amr_wb::depacketize(&current.codec.encode(&pcm).await.unwrap(), true)
            .unwrap()
            .mode,
        5
    );
    // Period-only update retains odd frame phase and the current mode.
    candidate.codec = AudioCodec::AmrWb {
        octet_aligned: true,
        mode: 5,
        policy: AmrWbModePolicy::new((1 << 0) | (1 << 5) | (1 << 8), 2, false).unwrap(),
    };
    media.prepare(candidate.clone()).await.unwrap();
    media.configure(candidate.clone()).await.unwrap();
    let current = media.current.as_mut().unwrap();
    assert_eq!(current.codec.encoded_frames, 3);
    current.codec.requested_mode = Some(0);
    assert_eq!(
        amr_wb::depacketize(&current.codec.encode(&pcm).await.unwrap(), true)
            .unwrap()
            .mode,
        5
    );
    assert_eq!(
        amr_wb::depacketize(&current.codec.encode(&pcm).await.unwrap(), true)
            .unwrap()
            .mode,
        0
    );
    assert!(!media.configure(candidate.clone()).await.unwrap());
    // Framing cannot be rebound to a PT that has already been used.
    let mut invalid = candidate.clone();
    invalid.codec = AudioCodec::AmrWb {
        octet_aligned: false,
        mode: 5,
        policy: AmrWbModePolicy::single(5).unwrap(),
    };
    assert_eq!(
        media.prepare(invalid).await.unwrap_err().code,
        "ims_media_payload_rebound"
    );
    assert_eq!(media.current.as_ref().unwrap().remote, candidate);
    let mut cross = candidate.clone();
    cross.payload_type = 96;
    cross.codec = AudioCodec::Evs { header_full: false };
    media.prepare(cross.clone()).await.unwrap();
    let staged = Arc::downgrade(
        media
            .pending
            .as_ref()
            .unwrap()
            .codec
            .native
            .as_ref()
            .unwrap(),
    );
    let mut other = cross;
    other.remote_port += 1;
    assert_eq!(
        media.prepare(other).await.unwrap_err().code,
        "ims_media_negotiation_pending"
    );
    media.discard().await.unwrap();
    assert!(
        staged.upgrade().is_none(),
        "discard joins the unshared staged native worker"
    );
    // Retired receive contexts expire without depending on future packets.
    for (_, deadline) in &mut media.retired {
        *deadline = Instant::now();
    }
    media.expire().await.unwrap();
    assert!(media.retired.is_empty());
    drop(native);
    let active = Arc::downgrade(
        media
            .current
            .as_ref()
            .unwrap()
            .codec
            .native
            .as_ref()
            .unwrap(),
    );
    media.close().await.unwrap();
    assert!(active.upgrade().is_none());
}

#[tokio::test]
async fn actual_voiced_rtp_renegotiates_evs_amr_policy_framing_and_back_without_reset() {
    use proletarius_call::sdp::AmrWbModePolicy;
    let config = |ip: &str| TunnelConfiguration {
        ipv4_addresses: vec![ip.parse().unwrap()],
        ..Default::default()
    };
    let a = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
    let b = TunnelNetwork::new(&config("10.0.0.3")).unwrap();
    let (ab_tx, ab_rx) = mpsc::channel(256);
    let (ba_tx, ba_rx) = mpsc::channel(256);
    let (stop, stop_rx) = watch::channel(false);
    let (wa, ra) = watch::channel(0);
    let (wb, rb) = watch::channel(0);
    let (ca, sa) = UdpSocketRegistry::new(wa);
    let (cb, sb) = UdpSocketRegistry::new(wb);
    let client = tokio::spawn(a.run_managed(
        SocketSet::new(vec![]),
        ba_rx,
        ab_tx,
        stop_rx.clone(),
        ra,
        sa,
    ));
    let server = tokio::spawn(b.run_managed(SocketSet::new(vec![]), ab_rx, ba_tx, stop_rx, rb, sb));
    let generation = SessionGeneration::from_value(701).unwrap();
    let binding = CallBinding {
        generation,
        call_id: "actual-voiced-renegotiation".into(),
        invite_cseq: 1,
    };
    let owner = Owner::new(generation, "10.0.0.2".parse().unwrap(), ca);
    let local = owner.clone().allocate(binding.clone()).await.unwrap();
    let mut remote = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
    let mut wrong = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
    let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let bridge = SocketAddr::new("127.0.0.1".parse().unwrap(), local.audio_port);
    let destination = SocketAddr::new(local.ip, local.port);
    let prefix = format!("{}\0", local.audio_token).into_bytes();
    host.send_to(&prefix, bridge).await.unwrap();
    let configs = [
        (96, AudioCodec::Evs { header_full: false }),
        (
            98,
            AudioCodec::AmrWb {
                octet_aligned: true,
                mode: 8,
                policy: AmrWbModePolicy::new((1 << 2) | (1 << 8), 1, false).unwrap(),
            },
        ),
        (
            98,
            AudioCodec::AmrWb {
                octet_aligned: true,
                mode: 2,
                policy: AmrWbModePolicy::single(2).unwrap(),
            },
        ),
        (
            99,
            AudioCodec::AmrWb {
                octet_aligned: false,
                mode: 2,
                policy: AmrWbModePolicy::single(2).unwrap(),
            },
        ),
        (100, AudioCodec::Evs { header_full: true }),
        (96, AudioCodec::Evs { header_full: false }),
        // The same EVS PT may narrow its permitted form to HF-only and then
        // reopen compact reception; this does not change codec identity.
        (96, AudioCodec::Evs { header_full: true }),
        (96, AudioCodec::Evs { header_full: false }),
    ];
    let mut previous = None;
    let mut received = 0;
    let mut incoming_sequence = 200u16;
    let mut saved_old = None;
    let mut saved_full = None;
    let mut pcm_in = [0u8; 2048];
    for (step, (pt, codec)) in configs.into_iter().enumerate() {
        let negotiation = AudioNegotiation {
            remote_ip: remote.local_addr().ip(),
            remote_port: remote.local_addr().port(),
            payload_type: pt,
            codec,
            telephone_event_payload: Some(102),
            telephone_event_mask: 0xffff,
        };
        let mut counterpart = CodecWorker::new(codec).await.unwrap(); // no capability skip
        if step == 0 {
            owner
                .clone()
                .configure(binding.clone(), negotiation.clone())
                .await
                .unwrap();
            owner.clone().start(binding.clone(), false).await.unwrap();
        } else {
            owner
                .clone()
                .prepare(binding.clone(), negotiation.clone())
                .await
                .unwrap();
            owner
                .clone()
                .prepare(binding.clone(), negotiation.clone())
                .await
                .unwrap();
            // New receive is ready before the successful answer/commit.
            let payload = counterpart.encode(&voiced_pcm(step * 10)).await.unwrap();
            remote
                .sender()
                .send_to(
                    rtp_packet(
                        pt,
                        incoming_sequence,
                        u32::from(incoming_sequence) * 320,
                        777,
                        &payload,
                    ),
                    destination,
                )
                .await
                .unwrap();
            incoming_sequence = incoming_sequence.wrapping_add(1);
            let (count, _) =
                tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut pcm_in))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(count, prefix.len() + 640);
            received += 1;
            owner
                .clone()
                .configure(binding.clone(), negotiation.clone())
                .await
                .unwrap();
            owner
                .clone()
                .configure(binding.clone(), negotiation)
                .await
                .unwrap();
        }
        for frame in 0..4 {
            let voiced = voiced_pcm(step * 10 + frame);
            let mut host_frame = prefix.clone();
            host_frame.extend_from_slice(&voiced);
            host.send_to(&host_frame, bridge).await.unwrap();
            let packet = tokio::time::timeout(Duration::from_secs(2), remote.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                packet.peer, destination,
                "same RTP socket across every transition"
            );
            let parsed = RtpPacket::parse(&packet.bytes).unwrap();
            assert_eq!(parsed.payload_type, pt);
            if let Some((ssrc, seq, timestamp)) = previous {
                assert_eq!(
                    (parsed.ssrc, parsed.sequence, parsed.timestamp),
                    (
                        ssrc,
                        u16::wrapping_add(seq, 1),
                        u32::wrapping_add(timestamp, 320)
                    )
                );
            }
            previous = Some((parsed.ssrc, parsed.sequence, parsed.timestamp));
            if let AudioCodec::AmrWb {
                octet_aligned,
                policy,
                ..
            } = codec
            {
                assert!(
                    policy.contains(
                        amr_wb::depacketize(parsed.payload, octet_aligned)
                            .unwrap()
                            .mode
                    )
                );
            }
            if let AudioCodec::Evs { header_full } = codec {
                assert_eq!(
                    parsed.payload.len(),
                    if header_full { 34 } else { 33 },
                    "same-PT hf-only updates change the actual transmitted payload form"
                );
                assert_eq!(
                    proletarius_call::evs::depacketize_primary(parsed.payload, header_full)
                        .unwrap()
                        .mode,
                    4
                );
            }
            let decoded = counterpart.decode(parsed.payload).await.unwrap();
            assert_eq!(decoded.len(), 640);
            assert!(
                decoded
                    .chunks_exact(2)
                    .any(|s| i16::from_le_bytes([s[0], s[1]]) != 0),
                "voiced output independently decodes"
            );
            let incoming = counterpart.encode(&voiced).await.unwrap();
            let wire = rtp_packet(
                pt,
                incoming_sequence,
                u32::from(incoming_sequence) * 320,
                777,
                &incoming,
            );
            // A wrong endpoint may not take the valid packet's sequence/CMR.
            wrong
                .sender()
                .send_to(wire.clone(), destination)
                .await
                .unwrap();
            remote
                .sender()
                .send_to(wire.clone(), destination)
                .await
                .unwrap();
            incoming_sequence = incoming_sequence.wrapping_add(1);
            let (count, _) =
                tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut pcm_in))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(count, prefix.len() + 640);
            assert_eq!(&pcm_in[..prefix.len()], &prefix);
            received += 1;
            assert!(
                pcm_in[prefix.len()..count]
                    .chunks_exact(2)
                    .any(|s| i16::from_le_bytes([s[0], s[1]]) != 0)
            );
            // Duplicate and late packets on a different PT cannot replay after
            // changing the policy because the endpoint sequence gate persists.
            remote.sender().send_to(wire, destination).await.unwrap();
            if step == 0 {
                saved_old = Some((pt, incoming));
            } else if step == 6 {
                saved_full = Some((pt, incoming));
            }
        }
        if step == 1 {
            let (old_pt, old) = saved_old.as_ref().unwrap();
            remote
                .sender()
                .send_to(
                    rtp_packet(
                        *old_pt,
                        incoming_sequence,
                        u32::from(incoming_sequence) * 320,
                        777,
                        old,
                    ),
                    destination,
                )
                .await
                .unwrap();
            incoming_sequence = incoming_sequence.wrapping_add(1);
            let (count, _) =
                tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut pcm_in))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(count, prefix.len() + 640);
            received += 1;
        }
        if step == 6 || step == 7 {
            let (old_pt, old) = if step == 6 {
                saved_old.as_ref().unwrap()
            } else {
                saved_full.as_ref().unwrap()
            };
            assert_eq!(*old_pt, 96);
            if step == 6 {
                assert!(
                    CodecWorker::frame(codec, old).is_err(),
                    "HF-only active receiver rejects compact; retired compact receiver provides bounded transition tolerance"
                );
            } else {
                assert!(
                    CodecWorker::frame(codec, old).is_ok(),
                    "hf-only=0 accepts full-header frames as well as compact"
                );
            }
            remote
                .sender()
                .send_to(
                    rtp_packet(
                        *old_pt,
                        incoming_sequence,
                        u32::from(incoming_sequence) * 320,
                        777,
                        old,
                    ),
                    destination,
                )
                .await
                .unwrap();
            incoming_sequence = incoming_sequence.wrapping_add(1);
            let (count, _) =
                tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut pcm_in))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(count, prefix.len() + 640);
            assert_eq!(&pcm_in[..prefix.len()], &prefix);
            received += 1;
        }
        counterpart.close().await.unwrap();
    }
    // A rejected mapping and a discarded replacement leave active EVS intact.
    let bad = AudioNegotiation {
        remote_ip: remote.local_addr().ip(),
        remote_port: remote.local_addr().port(),
        payload_type: 96,
        codec: AudioCodec::AmrWb {
            octet_aligned: true,
            mode: 2,
            policy: AmrWbModePolicy::single(2).unwrap(),
        },
        telephone_event_payload: Some(102),
        telephone_event_mask: 0xffff,
    };
    assert_eq!(
        owner
            .clone()
            .prepare(binding.clone(), bad)
            .await
            .unwrap_err()
            .code,
        "ims_media_payload_rebound"
    );
    let candidate = AudioNegotiation {
        remote_ip: remote.local_addr().ip(),
        remote_port: remote.local_addr().port(),
        payload_type: 98,
        codec: configs[1].1,
        telephone_event_payload: Some(102),
        telephone_event_mask: 0xffff,
    };
    owner
        .clone()
        .prepare(binding.clone(), candidate)
        .await
        .unwrap();
    let stale = CallBinding {
        call_id: "old-call".into(),
        ..binding.clone()
    };
    assert!(owner.clone().discard(stale.clone()).await.is_err());
    assert!(
        owner
            .clone()
            .prepare(
                stale,
                AudioNegotiation {
                    remote_ip: remote.local_addr().ip(),
                    remote_port: remote.local_addr().port(),
                    payload_type: 96,
                    codec: configs[0].1,
                    telephone_event_payload: None,
                    telephone_event_mask: 0,
                }
            )
            .await
            .is_err()
    );
    owner.clone().discard(binding.clone()).await.unwrap();
    let stats = owner.statistics(&binding);
    assert_eq!(stats.rtp_packets_sent, 32);
    assert_eq!(stats.rtp_packets_received, received);
    assert_eq!(stats.audio_frames_to_host, received);
    assert_eq!(stats.rtp_decode_errors, 0);
    owner.clone().stop(binding.clone()).await.unwrap();
    assert!(owner.state.lock().await.is_none());
    remote.close().await.unwrap();
    wrong.close().await.unwrap();
    stop.send_replace(true);
    client.await.unwrap().unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn returning_to_amr_uses_latest_native_mode_and_frame_phase() {
    use proletarius_call::sdp::AmrWbModePolicy;
    let base = AudioNegotiation {
        remote_ip: "10.0.0.3".parse().unwrap(),
        remote_port: 4000,
        payload_type: 98,
        telephone_event_payload: None,
        telephone_event_mask: 0,
        codec: AudioCodec::AmrWb {
            octet_aligned: true,
            mode: 8,
            policy: AmrWbModePolicy::single(8).unwrap(),
        },
    };
    let mut media = NegotiationState::default();
    media.configure(base.clone()).await.unwrap();
    let pcm = voiced_pcm(1);
    assert_eq!(
        amr_wb::depacketize(
            &media
                .current
                .as_mut()
                .unwrap()
                .codec
                .encode(&pcm)
                .await
                .unwrap(),
            true
        )
        .unwrap()
        .mode,
        8
    );
    let native = Arc::downgrade(
        media
            .current
            .as_ref()
            .unwrap()
            .codec
            .native
            .as_ref()
            .unwrap(),
    );
    let mut narrow = base.clone();
    narrow.codec = AudioCodec::AmrWb {
        octet_aligned: true,
        mode: 2,
        policy: AmrWbModePolicy::single(2).unwrap(),
    };
    media.configure(narrow).await.unwrap();
    assert_eq!(
        amr_wb::depacketize(
            &media
                .current
                .as_mut()
                .unwrap()
                .codec
                .encode(&pcm)
                .await
                .unwrap(),
            true
        )
        .unwrap()
        .mode,
        2
    );
    assert_eq!(media.current.as_ref().unwrap().codec.encoded_frames, 2);
    let mut evs = base.clone();
    evs.payload_type = 96;
    evs.codec = AudioCodec::Evs { header_full: false };
    media.configure(evs).await.unwrap();
    media
        .current
        .as_mut()
        .unwrap()
        .codec
        .encode(&pcm)
        .await
        .unwrap();
    media.prepare(base.clone()).await.unwrap();
    let pending = media.pending.as_ref().unwrap();
    assert!(Arc::ptr_eq(
        &native.upgrade().unwrap(),
        pending.codec.native.as_ref().unwrap()
    ));
    assert_eq!(
        pending.codec.transmit_mode, 2,
        "oldest retired WB metadata still says8 but native encoder now has2"
    );
    assert_eq!(
        pending.codec.encoded_frames, 2,
        "same-family frame phase survives the EVS interval"
    );
    media.configure(base).await.unwrap();
    let current = media.current.as_mut().unwrap();
    let actual = current.codec.encode(&pcm).await.unwrap();
    assert_eq!(
        amr_wb::depacketize(&actual, true).unwrap().mode,
        8,
        "first returned WB frame must belong to the newly accepted singleton set"
    );
    assert_eq!(current.codec.encoded_frames, 3);
    media.close().await.unwrap();
    assert!(native.upgrade().is_none());
}

#[tokio::test]
async fn policy_commit_keeps_queued_old_and_prepared_packets_with_their_wire_policy() {
    use proletarius_call::sdp::AmrWbModePolicy;
    let old = AudioNegotiation {
        remote_ip: "10.0.0.3".parse().unwrap(),
        remote_port: 4000,
        payload_type: 98,
        telephone_event_payload: None,
        telephone_event_mask: 0,
        codec: AudioCodec::AmrWb {
            octet_aligned: true,
            mode: 8,
            policy: AmrWbModePolicy::single(8).unwrap(),
        },
    };
    let mut media = NegotiationState::default();
    media.configure(old.clone()).await.unwrap();
    let old_payload = media
        .current
        .as_mut()
        .unwrap()
        .codec
        .encode(&voiced_pcm(0))
        .await
        .unwrap();
    let old_wire = rtp_packet(98, 100, 32000, 777, &old_payload);
    let old_receiver = media.current.as_ref().unwrap();
    let old_id = old_receiver.id;
    let ingress = old_receiver.ingress.clone();
    ingress.lock().unwrap().push(
        old_id,
        &RtpPacket::parse(&old_wire).unwrap(),
        std::time::Instant::now(),
    );
    let mut new = old;
    new.codec = AudioCodec::AmrWb {
        octet_aligned: true,
        mode: 8,
        policy: AmrWbModePolicy::new((1 << 2) | (1 << 8), 1, false).unwrap(),
    };
    media.prepare(new.clone()).await.unwrap();
    let mut sender = CodecWorker::new(AudioCodec::AmrWb {
        octet_aligned: true,
        mode: 2,
        policy: AmrWbModePolicy::single(2).unwrap(),
    })
    .await
    .unwrap();
    let new_payload = sender.encode(&voiced_pcm(1)).await.unwrap();
    let new_wire = rtp_packet(98, 101, 32320, 777, &new_payload);
    let pending = media.pending.as_ref().unwrap();
    let new_id = pending.id;
    assert!(Arc::ptr_eq(&ingress, &pending.ingress));
    ingress.lock().unwrap().push(
        new_id,
        &RtpPacket::parse(&new_wire).unwrap(),
        std::time::Instant::now(),
    );
    media.configure(new).await.unwrap();
    let (id, queued) = ingress
        .lock()
        .unwrap()
        .pop(std::time::Instant::now())
        .unwrap();
    assert_eq!((id, queued.sequence), (old_id, 100));
    let retired = &media.retired[0].0;
    assert!(CodecWorker::frame(retired.remote.codec, &queued.payload).is_ok());
    let (id, prepared) = ingress
        .lock()
        .unwrap()
        .pop(std::time::Instant::now())
        .unwrap();
    assert_eq!((id, prepared.sequence), (new_id, 101));
    assert!(
        CodecWorker::frame(
            media.current.as_ref().unwrap().remote.codec,
            &prepared.payload
        )
        .is_ok()
    );
    assert!(CodecWorker::frame(retired.remote.codec, &prepared.payload).is_err());
    assert!(Arc::ptr_eq(
        &retired.sequence,
        &media.current.as_ref().unwrap().sequence
    ));
    assert_eq!(
        retired.sequence.lock().unwrap().observe(777, 100),
        SequenceDisposition::First
    );
    assert_eq!(
        media
            .current
            .as_ref()
            .unwrap()
            .sequence
            .lock()
            .unwrap()
            .observe(777, 101),
        SequenceDisposition::InOrder
    );
    assert_eq!(
        retired.sequence.lock().unwrap().observe(777, 100),
        SequenceDisposition::DuplicateOrLate
    );
    sender.close().await.unwrap();
    media.close().await.unwrap();
}

#[test]
fn interrupted_dtmf_resumes_the_shared_clock_after_only_sent_intervals() {
    let start = u32::MAX - 500;
    let (packets, _, next_timestamp) =
        proletarius_call::dtmf_rtp_schedule(1, 2, 102, 10, start, 42).unwrap();
    for sent in 0..=packets.len() {
        let (reply, _) = oneshot::channel();
        let job = Dtmf {
            packets: packets.clone(),
            index: sent,
            next: Instant::now(),
            next_timestamp,
            reply,
        };
        assert_eq!(
            job.resume_timestamp(),
            start.wrapping_add(sent as u32 * 320)
        );
    }
}

#[tokio::test]
async fn shared_endpoint_reorder_decodes_old_and_new_pt_in_sequence_without_false_plc() {
    use proletarius_call::sdp::AmrWbModePolicy;
    let amr = AudioCodec::AmrWb {
        octet_aligned: true,
        mode: 8,
        policy: AmrWbModePolicy::single(8).unwrap(),
    };
    let evs = AudioCodec::Evs { header_full: false };
    let old = AudioNegotiation {
        remote_ip: "10.0.0.3".parse().unwrap(),
        remote_port: 4000,
        payload_type: 98,
        codec: amr,
        telephone_event_payload: None,
        telephone_event_mask: 0,
    };
    let mut media = NegotiationState::default();
    media.configure(old.clone()).await.unwrap();
    let source = media.current.as_ref().unwrap().endpoint();
    let old_id = media.current.as_ref().unwrap().id;
    let ingress = media.current.as_ref().unwrap().ingress.clone();
    let mut old_encoder = CodecWorker::new(amr).await.unwrap();
    let mut old_reference = CodecWorker::new(amr).await.unwrap();
    let mut new_encoder = CodecWorker::new(evs).await.unwrap();
    let mut new_reference = CodecWorker::new(evs).await.unwrap();
    let mut payloads = vec![];
    let mut expected = vec![];
    for frame in 0..3 {
        let payload = old_encoder.encode(&voiced_pcm(frame)).await.unwrap();
        expected.push(old_reference.decode(&payload).await.unwrap());
        payloads.push(payload);
    }
    let new_payload = new_encoder.encode(&voiced_pcm(3)).await.unwrap();
    expected.push(new_reference.decode(&new_payload).await.unwrap());
    let audio = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let prefix = b"fixed-test-token\0";
    let peer = Some(host.local_addr().unwrap());
    let stats = Mutex::new(Stats::default());
    let packet100 = rtp_packet(98, 100, 32000, 777, &payloads[0]);
    let parsed = RtpPacket::parse(&packet100).unwrap();
    assert!(media.current.as_ref().unwrap().accepts(source, &parsed));
    ingress
        .lock()
        .unwrap()
        .push(old_id, &parsed, std::time::Instant::now());
    receive_ready(&mut media, &audio, prefix, peer, &stats)
        .await
        .unwrap();
    let mut output = [0u8; 2048];
    let (count, _) = tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut output))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&output[prefix.len()..count], &expected[0]);
    // Allocate the next actual codec before starting the 40ms reorder gap;
    // scheduling native construction must not make this regression time-based.
    let mut candidate = old;
    candidate.payload_type = 96;
    candidate.codec = evs;
    media.prepare(candidate.clone()).await.unwrap();
    let packet102 = rtp_packet(98, 102, 32640, 777, &payloads[2]);
    ingress.lock().unwrap().push(
        old_id,
        &RtpPacket::parse(&packet102).unwrap(),
        std::time::Instant::now(),
    );
    receive_ready(&mut media, &audio, prefix, peer, &stats)
        .await
        .unwrap();
    assert_eq!(stats.lock().unwrap().audio_frames_to_host, 1);
    let new_id = media.pending.as_ref().unwrap().id;
    assert!(Arc::ptr_eq(
        &ingress,
        &media.pending.as_ref().unwrap().ingress
    ));
    media.configure(candidate).await.unwrap();
    let packet103 = rtp_packet(96, 103, 32960, 777, &new_payload);
    let parsed = RtpPacket::parse(&packet103).unwrap();
    assert!(media.current.as_ref().unwrap().accepts(source, &parsed));
    ingress
        .lock()
        .unwrap()
        .push(new_id, &parsed, std::time::Instant::now());
    receive_ready(&mut media, &audio, prefix, peer, &stats)
        .await
        .unwrap();
    assert_eq!(
        stats.lock().unwrap().audio_frames_to_host,
        1,
        "new PT103 must wait for old PT101 and102"
    );
    let packet101 = rtp_packet(98, 101, 32320, 777, &payloads[1]);
    ingress.lock().unwrap().push(
        old_id,
        &RtpPacket::parse(&packet101).unwrap(),
        std::time::Instant::now(),
    );
    // Retirement stops further old admission, but already queued old frames
    // retain their exact decoder until the common ingress has drained them.
    media.retired[0].1 = Instant::now();
    media.expire().await.unwrap();
    assert_eq!(media.retired.len(), 1);
    receive_ready(&mut media, &audio, prefix, peer, &stats)
        .await
        .unwrap();
    for reference in &expected[1..] {
        let (count, _) = tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut output))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&output[..prefix.len()], prefix);
        assert_eq!(
            &output[prefix.len()..count],
            reference,
            "actual independent codec output proves101,102,103 playout order"
        );
    }
    assert_eq!(stats.lock().unwrap().rtp_packets_received, 4);
    assert_eq!(
        stats.lock().unwrap().audio_frames_to_host,
        4,
        "no synthetic PLC or old-packet loss"
    );
    assert_eq!(stats.lock().unwrap().rtp_decode_errors, 0);
    assert!(ingress.lock().unwrap().receiver_ids.is_empty());
    media.expire().await.unwrap();
    assert!(media.retired.is_empty());
    old_encoder.close().await.unwrap();
    old_reference.close().await.unwrap();
    new_encoder.close().await.unwrap();
    new_reference.close().await.unwrap();
    media.close().await.unwrap();
}

#[tokio::test]
async fn native_tx_state_survives_latest_receiver_expiry_while_older_rx_queue_is_retained() {
    use proletarius_call::sdp::AmrWbModePolicy;
    let base = AudioNegotiation {
        remote_ip: "10.0.0.3".parse().unwrap(),
        remote_port: 4000,
        payload_type: 98,
        telephone_event_payload: None,
        telephone_event_mask: 0,
        codec: AudioCodec::AmrWb {
            octet_aligned: true,
            mode: 8,
            policy: AmrWbModePolicy::single(8).unwrap(),
        },
    };
    let mut media = NegotiationState::default();
    media.configure(base.clone()).await.unwrap();
    let pcm = voiced_pcm(0);
    let old_payload = media
        .current
        .as_mut()
        .unwrap()
        .codec
        .encode(&pcm)
        .await
        .unwrap();
    assert_eq!(amr_wb::depacketize(&old_payload, true).unwrap().mode, 8);
    let oldest_id = media.current.as_ref().unwrap().id;
    let ingress = media.current.as_ref().unwrap().ingress.clone();
    let native = Arc::downgrade(
        media
            .current
            .as_ref()
            .unwrap()
            .codec
            .native
            .as_ref()
            .unwrap(),
    );
    let mut narrow = base.clone();
    narrow.codec = AudioCodec::AmrWb {
        octet_aligned: true,
        mode: 2,
        policy: AmrWbModePolicy::single(2).unwrap(),
    };
    media.configure(narrow).await.unwrap();
    let narrowed = media
        .current
        .as_mut()
        .unwrap()
        .codec
        .encode(&pcm)
        .await
        .unwrap();
    assert_eq!(amr_wb::depacketize(&narrowed, true).unwrap().mode, 2);
    let mut evs = base.clone();
    evs.payload_type = 96;
    evs.codec = AudioCodec::Evs { header_full: false };
    media.configure(evs).await.unwrap();
    assert_eq!(media.retired.len(), 2);
    // Only the oldest WB receiver has queued audio at retirement. Its local
    // TX cache says8/frame1; the shared native encoder has since sent2/frame2.
    let queued = rtp_packet(98, 100, 32000, 777, &old_payload);
    ingress.lock().unwrap().push(
        oldest_id,
        &RtpPacket::parse(&queued).unwrap(),
        std::time::Instant::now(),
    );
    for (_, deadline) in &mut media.retired {
        *deadline = Instant::now();
    }
    media.expire().await.unwrap();
    assert_eq!(media.retired.len(), 1);
    assert_eq!(media.retired[0].0.id, oldest_id);
    assert_eq!(media.retired[0].0.codec.transmit_mode, 8);
    assert_eq!(media.retired[0].0.codec.encoded_frames, 1);
    media.prepare(base.clone()).await.unwrap();
    let pending = media.pending.as_ref().unwrap();
    assert!(Arc::ptr_eq(
        &native.upgrade().unwrap(),
        pending.codec.native.as_ref().unwrap()
    ));
    assert_eq!(
        (pending.codec.transmit_mode, pending.codec.encoded_frames),
        (2, 2),
        "native TX state remains authoritative after the latest receiver was removed"
    );
    media.configure(base).await.unwrap();
    let current = media.current.as_mut().unwrap();
    let returned = current.codec.encode(&voiced_pcm(1)).await.unwrap();
    assert_eq!(
        amr_wb::depacketize(&returned, true).unwrap().mode,
        8,
        "no FT2 frame may escape the newly accepted singleton8 set"
    );
    assert_eq!(current.codec.encoded_frames, 3);
    let (queued_id, queued) = ingress
        .lock()
        .unwrap()
        .pop(std::time::Instant::now())
        .unwrap();
    assert_eq!(queued_id, oldest_id);
    let old_receiver = media
        .retired
        .iter_mut()
        .find(|(r, _)| r.id == oldest_id)
        .unwrap();
    assert_eq!(
        old_receiver.0.remote.codec,
        AudioCodec::AmrWb {
            octet_aligned: true,
            mode: 8,
            policy: AmrWbModePolicy::single(8).unwrap(),
        }
    );
    assert_eq!(
        old_receiver
            .0
            .codec
            .decode(&queued.payload)
            .await
            .unwrap()
            .len(),
        640,
        "authoritative TX updates do not rewrite the retained RX parser"
    );
    media.expire().await.unwrap();
    media.close().await.unwrap();
    assert!(native.upgrade().is_none());
}

#[tokio::test]
async fn initial_prepare_receives_actual_rtp_before_200_and_ack_without_premature_transmit() {
    use proletarius_call::sdp::AmrWbModePolicy;
    for codec in [
        AudioCodec::Evs { header_full: false },
        AudioCodec::AmrWb {
            octet_aligned: true,
            mode: 2,
            policy: AmrWbModePolicy::single(2).unwrap(),
        },
    ] {
        let config = |ip: &str| TunnelConfiguration {
            ipv4_addresses: vec![ip.parse().unwrap()],
            ..Default::default()
        };
        let a = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
        let b = TunnelNetwork::new(&config("10.0.0.3")).unwrap();
        let (ab_tx, ab_rx) = mpsc::channel(256);
        let (ba_tx, ba_rx) = mpsc::channel(256);
        let (stop, stop_rx) = watch::channel(false);
        let (wa, ra) = watch::channel(0);
        let (wb, rb) = watch::channel(0);
        let (ca, sa) = UdpSocketRegistry::new(wa);
        let (cb, sb) = UdpSocketRegistry::new(wb);
        let client = tokio::spawn(a.run_managed(
            SocketSet::new(vec![]),
            ba_rx,
            ab_tx,
            stop_rx.clone(),
            ra,
            sa,
        ));
        let server =
            tokio::spawn(b.run_managed(SocketSet::new(vec![]), ab_rx, ba_tx, stop_rx, rb, sb));
        let generation = SessionGeneration::from_value(901).unwrap();
        let binding = CallBinding {
            generation,
            call_id: "initial-receive-readiness".into(),
            invite_cseq: 1,
        };
        let owner = Owner::new(generation, "10.0.0.2".parse().unwrap(), ca);
        let local = owner.clone().allocate(binding.clone()).await.unwrap();
        let mut remote = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
        let negotiation = AudioNegotiation {
            remote_ip: remote.local_addr().ip(),
            remote_port: remote.local_addr().port(),
            payload_type: 96,
            codec,
            telephone_event_payload: Some(102),
            telephone_event_mask: 0xffff,
        };
        let mut peer_codec = CodecWorker::new(codec).await.unwrap();
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bridge = SocketAddr::new("127.0.0.1".parse().unwrap(), local.audio_port);
        let destination = SocketAddr::new(local.ip, local.port);
        let prefix = format!("{}\0", local.audio_token).into_bytes();
        host.send_to(&prefix, bridge).await.unwrap();
        owner
            .clone()
            .prepare(binding.clone(), negotiation.clone())
            .await
            .unwrap();
        let mut decoded = [0u8; 2048];
        for stage in 0..3 {
            if stage == 1 {
                // A successful200 may commit without authorizing voice TX;
                // matching ACK is represented by the final start(false).
                owner
                    .clone()
                    .configure(binding.clone(), negotiation.clone())
                    .await
                    .unwrap();
            } else if stage == 2 {
                owner.clone().start(binding.clone(), true).await.unwrap();
            }
            let voice = voiced_pcm(stage);
            let incoming = peer_codec.encode(&voice).await.unwrap();
            remote
                .sender()
                .send_to(
                    rtp_packet(
                        96,
                        100 + stage as u16,
                        32000 + stage as u32 * 320,
                        777,
                        &incoming,
                    ),
                    destination,
                )
                .await
                .unwrap();
            let (count, source) =
                tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut decoded))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(source, bridge);
            assert_eq!(count, prefix.len() + 640);
            assert_eq!(&decoded[..prefix.len()], &prefix);
            assert!(
                decoded[prefix.len()..count].iter().any(|byte| *byte != 0),
                "actual receive decoding remains ready in stage{stage}"
            );
            let mut outgoing = prefix.clone();
            outgoing.extend_from_slice(&voice);
            host.send_to(&outgoing, bridge).await.unwrap();
            assert!(owner.clone().dtmf(binding.clone(), '#').await.is_err());
            assert!(
                tokio::time::timeout(Duration::from_millis(20), remote.recv())
                    .await
                    .is_err(),
                "prepare/commit/preACK receive readiness cannot authorize voice or DTMF TX in stage{stage}"
            );
            assert_eq!(owner.statistics(&binding).rtp_packets_sent, 0);
        }
        owner.clone().start(binding.clone(), false).await.unwrap();
        let mut outgoing = prefix.clone();
        outgoing.extend_from_slice(&voiced_pcm(3));
        host.send_to(&outgoing, bridge).await.unwrap();
        let packet = tokio::time::timeout(Duration::from_secs(2), remote.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(packet.peer, destination);
        let parsed = RtpPacket::parse(&packet.bytes).unwrap();
        assert_eq!(parsed.payload_type, 96);
        assert_eq!(peer_codec.decode(parsed.payload).await.unwrap().len(), 640);
        let (ssrc, sequence, timestamp) = (parsed.ssrc, parsed.sequence, parsed.timestamp);
        owner.clone().dtmf(binding.clone(), '#').await.unwrap();
        for index in 0..7 {
            let event = remote.recv().await.unwrap();
            let parsed = RtpPacket::parse(&event.bytes).unwrap();
            assert_eq!(parsed.payload_type, 102);
            assert_eq!(parsed.payload[0], 11);
            assert_eq!(parsed.ssrc, ssrc);
            assert_eq!(parsed.sequence, sequence.wrapping_add(1 + index));
            assert_eq!(parsed.timestamp, timestamp.wrapping_add(320));
        }
        let stats = owner.statistics(&binding);
        assert_eq!(stats.rtp_packets_sent, 8);
        assert_eq!(stats.rtp_packets_received, 3);
        assert_eq!(stats.audio_frames_to_host, 3);
        assert_eq!(stats.rtp_decode_errors, 0);
        peer_codec.close().await.unwrap();
        owner.clone().stop(binding.clone()).await.unwrap();
        assert!(owner.state.lock().await.is_none());
        assert!(owner.clone().start(binding, false).await.is_err());
        remote.close().await.unwrap();
        stop.send_replace(true);
        client.await.unwrap().unwrap();
        server.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn failed_media_stop_retains_the_allocation_fence_and_cannot_release_transmit() {
    let generation = SessionGeneration::from_value(902).unwrap();
    let binding = CallBinding {
        generation,
        call_id: "failed-stop-fence".into(),
        invite_cseq: 1,
    };
    let (wake, _) = watch::channel(0);
    let (udp, _registry) = UdpSocketRegistry::new(wake);
    let owner = Owner::new(generation, "10.0.0.2".parse().unwrap(), udp);
    let (commands, _input) = mpsc::channel(1);
    let (stop, stopping) = watch::channel(false);
    let (_done, done) = watch::channel(Some(Err(Failure::known(
        "ims_media_test_cleanup_unconfirmed",
    ))));
    *owner.state.lock().await = Some(Entry {
        binding: binding.clone(),
        commands,
        stop,
        done,
        stats: Arc::new(Mutex::new(Stats::default())),
    });
    for _ in 0..2 {
        assert_eq!(
            owner.clone().stop(binding.clone()).await.unwrap_err().code,
            "ims_media_test_cleanup_unconfirmed"
        );
        assert!(*stopping.borrow());
        assert!(
            owner
                .state
                .lock()
                .await
                .as_ref()
                .is_some_and(|entry| entry.binding == binding)
        );
    }
    let next = CallBinding {
        call_id: "replacement-call".into(),
        ..binding.clone()
    };
    assert_eq!(
        owner.clone().allocate(next).await.err().unwrap().code,
        "ims_media_busy"
    );
    assert_eq!(
        owner
            .clone()
            .hold_transmit(binding, false)
            .await
            .unwrap_err()
            .code,
        "ims_media_worker_stopped"
    );
}

fn initial_dual_local_offer(ip: IpAddr, port: u16) -> Vec<u8> {
    sdp::build_local_sdp_version(11, 1, ip, port, false, None, None, Some(102), false)
        .unwrap()
        .into_bytes()
}

#[tokio::test]
async fn local_offer_prepares_and_reuses_actual_codecs_without_a_remote_endpoint() {
    for payload_type in [96, 98] {
        let offer = initial_dual_local_offer("10.0.0.2".parse().unwrap(), 49170);
        let mut media = NegotiationState::default();
        media.prepare_local_offer(offer.clone()).await.unwrap();
        assert!(media.current.is_none());
        assert!(media.pending.is_none());
        let prepared = media.local_offer.as_ref().unwrap();
        assert_eq!(prepared.formats.len(), 2);
        let workers = prepared
            .formats
            .iter()
            .map(|format| {
                assert_eq!(format.codec.encoded_frames, 0);
                (
                    format.payload_type,
                    Arc::downgrade(format.codec.native.as_ref().unwrap()),
                )
            })
            .collect::<Vec<_>>();
        let codec = prepared
            .formats
            .iter()
            .find(|format| format.payload_type == payload_type)
            .unwrap()
            .codec
            .codec;
        let codec = match codec {
            AudioCodec::AmrWb { octet_aligned, .. } => AudioCodec::AmrWb {
                octet_aligned,
                mode: 2,
                policy: proletarius_call::sdp::AmrWbModePolicy::single(2).unwrap(),
            },
            evs => evs,
        };
        media.prepare_local_offer(offer.clone()).await.unwrap();
        for (payload, native) in &workers {
            let format = media
                .local_offer
                .as_ref()
                .unwrap()
                .formats
                .iter()
                .find(|format| format.payload_type == *payload)
                .unwrap();
            assert!(Arc::ptr_eq(
                &native.upgrade().unwrap(),
                format.codec.native.as_ref().unwrap()
            ));
        }
        assert!(
            media
                .prepare_local_offer(b"different-offer".to_vec())
                .await
                .is_err()
        );
        let remote = AudioNegotiation {
            remote_ip: "10.0.0.3".parse().unwrap(),
            remote_port: 49200,
            payload_type,
            codec,
            telephone_event_payload: Some(102),
            telephone_event_mask: 0xffff,
        };
        assert!(
            media
                .configure(AudioNegotiation {
                    payload_type: 120,
                    ..remote.clone()
                })
                .await
                .is_err()
        );
        assert!(
            media.local_offer.is_some(),
            "an invalid answer cannot consume the prepared codecs"
        );
        media.configure(remote).await.unwrap();
        assert!(media.local_offer.is_none());
        assert!(media.pending.is_none());
        for (payload, native) in &workers {
            if *payload == payload_type {
                assert!(Arc::ptr_eq(
                    &native.upgrade().unwrap(),
                    media
                        .current
                        .as_ref()
                        .unwrap()
                        .codec
                        .native
                        .as_ref()
                        .unwrap()
                ));
            } else {
                assert!(
                    native.upgrade().is_none(),
                    "unselected native worker must be joined on commit"
                );
            }
        }
        let frame = media
            .current
            .as_mut()
            .unwrap()
            .codec
            .encode(&voiced_pcm(0))
            .await
            .unwrap();
        if payload_type == 98 {
            assert_eq!(
                amr_wb::depacketize(&frame, true).unwrap().mode,
                2,
                "the prebuilt mode8 encoder must honor the narrower ACK on its first frame"
            );
        }
        media.close().await.unwrap();
        assert!(workers.iter().all(|(_, native)| native.upgrade().is_none()));
    }

    let offer = initial_dual_local_offer("10.0.0.2".parse().unwrap(), 49170);
    let mut media = NegotiationState::default();
    media.prepare_local_offer(offer.clone()).await.unwrap();
    let workers = media
        .local_offer
        .as_ref()
        .unwrap()
        .formats
        .iter()
        .map(|format| Arc::downgrade(format.codec.native.as_ref().unwrap()))
        .collect::<Vec<_>>();
    media.discard().await.unwrap();
    assert!(media.local_offer.is_none());
    assert!(workers.iter().all(|native| native.upgrade().is_none()));
    media.prepare_local_offer(offer).await.unwrap();
    media.close().await.unwrap();
    assert!(media.local_offer.is_none());

    let evs_only = sdp::build_local_sdp_version(
        11,
        1,
        "10.0.0.2".parse().unwrap(),
        49170,
        false,
        None,
        None,
        Some(102),
        true,
    )
    .unwrap()
    .into_bytes();
    media.prepare_local_offer(evs_only).await.unwrap();
    let prepared = media.local_offer.as_ref().unwrap();
    assert_eq!(prepared.formats.len(), 1);
    assert_eq!(prepared.formats[0].payload_type, 96);
    assert!(matches!(
        prepared.formats[0].codec.codec,
        AudioCodec::Evs { .. }
    ));
    media.close().await.unwrap();
}

#[tokio::test]
async fn local_offer_preanswer_window_is_bounded_and_never_runs_a_decoder_before_endpoint_validation()
 {
    let offer = initial_dual_local_offer("10.0.0.2".parse().unwrap(), 49170);
    let mut media = NegotiationState::default();
    media.prepare_local_offer(offer).await.unwrap();
    let codec = AudioCodec::Evs { header_full: false };
    let mut sender = CodecWorker::new(codec).await.unwrap();
    let payload = sender.encode(&voiced_pcm(0)).await.unwrap();
    let prepared = media.local_offer.as_mut().unwrap();
    for sequence in 0..100 {
        prepared.buffer(
            "10.0.0.3:49200".parse().unwrap(),
            &RtpPacket {
                marker: false,
                payload_type: 96,
                sequence,
                timestamp: sequence as u32 * 320,
                ssrc: 7,
                payload: &payload,
            },
        );
    }
    assert_eq!(prepared.packets.len(), MAX_PREANSWER_PACKETS);
    assert_eq!(
        prepared.packets.front().unwrap().packet.sequence,
        100 - MAX_PREANSWER_PACKETS as u16
    );
    assert!(
        prepared
            .formats
            .iter()
            .all(|format| format.codec.encoded_frames == 0 && !format.codec.received_cmr)
    );
    let snapshot = prepared.packets.len();
    for (payload_type, payload) in [(120, payload.as_slice()), (96, &[0u8][..])] {
        prepared.buffer(
            "10.0.0.3:49200".parse().unwrap(),
            &RtpPacket {
                marker: false,
                payload_type,
                sequence: 100,
                timestamp: 32000,
                ssrc: 7,
                payload,
            },
        );
        assert_eq!(prepared.packets.len(), snapshot);
        assert_eq!(prepared.packets.back().unwrap().packet.sequence, 99);
    }
    media.discard().await.unwrap();
    assert!(media.local_offer.is_none());
    sender.close().await.unwrap();
}

#[tokio::test]
async fn initial_offerless_actual_dual_codec_preparation_buffers_rtp_and_never_transmits_before_ack()
 {
    for payload_type in [96, 98] {
        let config = |ip: &str| TunnelConfiguration {
            ipv4_addresses: vec![ip.parse().unwrap()],
            ..Default::default()
        };
        let a = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
        let b = TunnelNetwork::new(&config("10.0.0.3")).unwrap();
        let (ab_tx, ab_rx) = mpsc::channel(256);
        let (ba_tx, ba_rx) = mpsc::channel(256);
        let (stop, stop_rx) = watch::channel(false);
        let (wa, ra) = watch::channel(0);
        let (wb, rb) = watch::channel(0);
        let (ca, sa) = UdpSocketRegistry::new(wa);
        let (cb, sb) = UdpSocketRegistry::new(wb);
        let client = tokio::spawn(a.run_managed(
            SocketSet::new(vec![]),
            ba_rx,
            ab_tx,
            stop_rx.clone(),
            ra,
            sa,
        ));
        let server =
            tokio::spawn(b.run_managed(SocketSet::new(vec![]), ab_rx, ba_tx, stop_rx, rb, sb));
        let generation = SessionGeneration::from_value(903).unwrap();
        let binding = CallBinding {
            generation,
            call_id: format!("initial-offerless-{payload_type}"),
            invite_cseq: 1,
        };
        let owner = Owner::new(generation, "10.0.0.2".parse().unwrap(), ca);
        let local = owner.clone().allocate(binding.clone()).await.unwrap();
        let offer = initial_dual_local_offer(local.ip, local.port);
        owner
            .clone()
            .prepare_local_offer(binding.clone(), offer.clone())
            .await
            .unwrap();
        owner
            .clone()
            .prepare_local_offer(binding.clone(), offer)
            .await
            .unwrap();
        let mut remote = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
        let mut other = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
        let codec = local_receive_formats(&initial_dual_local_offer(local.ip, local.port))
            .unwrap()
            .into_iter()
            .find(|(payload, _)| *payload == payload_type)
            .unwrap()
            .1;
        let mut peer_codec = CodecWorker::new(codec).await.unwrap();
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bridge = SocketAddr::new("127.0.0.1".parse().unwrap(), local.audio_port);
        let destination = SocketAddr::new(local.ip, local.port);
        let prefix = format!("{}\0", local.audio_token).into_bytes();
        let mut outgoing = prefix.clone();
        outgoing.extend_from_slice(&voiced_pcm(4));
        host.send_to(&outgoing, bridge).await.unwrap();
        assert!(owner.clone().dtmf(binding.clone(), '#').await.is_err());
        assert!(owner.clone().start(binding.clone(), false).await.is_err());
        let incoming = peer_codec.encode(&voiced_pcm(0)).await.unwrap();
        other
            .sender()
            .send_to(
                rtp_packet(payload_type, 50, 16000, 123, &incoming),
                destination,
            )
            .await
            .unwrap();
        let other_payload_type = if payload_type == 96 { 98 } else { 96 };
        let other_codec = local_receive_formats(&initial_dual_local_offer(local.ip, local.port))
            .unwrap()
            .into_iter()
            .find(|(payload, _)| *payload == other_payload_type)
            .unwrap()
            .1;
        let mut unselected_codec = CodecWorker::new(other_codec).await.unwrap();
        let unselected = unselected_codec.encode(&voiced_pcm(0)).await.unwrap();
        remote
            .sender()
            .send_to(
                rtp_packet(other_payload_type, 99, 31680, 777, &unselected),
                destination,
            )
            .await
            .unwrap();
        unselected_codec.close().await.unwrap();
        for frame in 0..4 {
            let incoming = peer_codec.encode(&voiced_pcm(frame)).await.unwrap();
            remote
                .sender()
                .send_to(
                    rtp_packet(
                        payload_type,
                        100 + frame as u16,
                        32000 + frame as u32 * 320,
                        777,
                        &incoming,
                    ),
                    destination,
                )
                .await
                .unwrap();
        }
        let mut decoded = [0; 2048];
        assert!(
            tokio::time::timeout(Duration::from_millis(30), host.recv_from(&mut decoded))
                .await
                .is_err(),
            "no endpoint is authenticated before the answer"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), remote.recv())
                .await
                .is_err()
        );
        assert_eq!(owner.statistics(&binding).rtp_packets_sent, 0);
        assert_eq!(owner.statistics(&binding).rtp_packets_received, 0);
        owner
            .clone()
            .configure(
                binding.clone(),
                AudioNegotiation {
                    remote_ip: remote.local_addr().ip(),
                    remote_port: remote.local_addr().port(),
                    payload_type,
                    codec,
                    telephone_event_payload: Some(102),
                    telephone_event_mask: 0xffff,
                },
            )
            .await
            .unwrap();
        for _ in 0..4 {
            let (count, source) =
                tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut decoded))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(source, bridge);
            assert_eq!(count, prefix.len() + 640);
            assert_eq!(&decoded[..prefix.len()], &prefix);
            assert!(decoded[prefix.len()..count].iter().any(|byte| *byte != 0));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), host.recv_from(&mut decoded))
                .await
                .is_err(),
            "the unselected source cannot enter the decoder"
        );
        host.send_to(&outgoing, bridge).await.unwrap();
        assert!(owner.clone().dtmf(binding.clone(), '#').await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), remote.recv())
                .await
                .is_err(),
            "configure does not grant transmit before start(false)"
        );
        owner.clone().start(binding.clone(), false).await.unwrap();
        host.send_to(&outgoing, bridge).await.unwrap();
        let packet = tokio::time::timeout(Duration::from_secs(2), remote.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(packet.peer, destination);
        let first = RtpPacket::parse(&packet.bytes).unwrap();
        assert_eq!(first.payload_type, payload_type);
        assert_eq!(peer_codec.decode(first.payload).await.unwrap().len(), 640);
        owner.clone().dtmf(binding.clone(), '#').await.unwrap();
        for index in 0..7 {
            let event = remote.recv().await.unwrap();
            let event = RtpPacket::parse(&event.bytes).unwrap();
            assert_eq!(event.payload_type, 102);
            assert_eq!(event.payload[0], 11);
            assert_eq!(event.ssrc, first.ssrc);
            assert_eq!(event.sequence, first.sequence.wrapping_add(1 + index));
            assert_eq!(event.timestamp, first.timestamp.wrapping_add(320));
        }
        host.send_to(&outgoing, bridge).await.unwrap();
        let packet = remote.recv().await.unwrap();
        let resumed = RtpPacket::parse(&packet.bytes).unwrap();
        assert_eq!(resumed.ssrc, first.ssrc);
        assert_eq!(resumed.sequence, first.sequence.wrapping_add(8));
        assert_eq!(resumed.timestamp, first.timestamp.wrapping_add(8 * 320));
        assert_eq!(peer_codec.decode(resumed.payload).await.unwrap().len(), 640);
        let stats = owner.statistics(&binding);
        assert_eq!(stats.rtp_packets_received, 4);
        assert_eq!(stats.audio_frames_to_host, 4);
        assert_eq!(stats.rtp_decode_errors, 0);
        assert_eq!(stats.rtp_packets_sent, 9);
        peer_codec.close().await.unwrap();
        owner.clone().stop(binding.clone()).await.unwrap();
        assert!(owner.state.lock().await.is_none());
        assert!(
            owner
                .clone()
                .prepare_local_offer(binding, initial_dual_local_offer(local.ip, local.port))
                .await
                .is_err()
        );
        remote.close().await.unwrap();
        other.close().await.unwrap();
        stop.send_replace(true);
        client.await.unwrap().unwrap();
        server.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn prehost_actual_pcm_queue_keeps_latest_32_frames_and_plc_decoder_history() {
    for codec in [
        AudioCodec::Evs { header_full: false },
        AudioCodec::AmrWb {
            octet_aligned: true,
            mode: 2,
            policy: proletarius_call::sdp::AmrWbModePolicy::single(2).unwrap(),
        },
    ] {
        let mut media = NegotiationState::default();
        media
            .configure(AudioNegotiation {
                remote_ip: "10.0.0.3".parse().unwrap(),
                remote_port: 49200,
                payload_type: 96,
                codec,
                telephone_event_payload: None,
                telephone_event_mask: 0,
            })
            .await
            .unwrap();
        let receiver = media.current.as_ref().unwrap();
        let id = receiver.id;
        let ingress = receiver.ingress.clone();
        let mut sender = CodecWorker::new(codec).await.unwrap();
        let mut reference = CodecWorker::new(codec).await.unwrap();
        let audio = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let prefix = b"prehost-capacity-token\0";
        let stats = Mutex::new(Stats::default());
        let mut expected = vec![];
        for frame in 0..40 {
            let payload = sender.encode(&voiced_pcm(frame)).await.unwrap();
            expected.push(reference.decode(&payload).await.unwrap());
            let bytes = rtp_packet(
                96,
                100 + frame as u16,
                32000 + frame as u32 * 320,
                777,
                &payload,
            );
            ingress.lock().unwrap().push(
                id,
                &RtpPacket::parse(&bytes).unwrap(),
                std::time::Instant::now(),
            );
            receive_ready(&mut media, &audio, prefix, None, &stats)
                .await
                .unwrap();
            assert_eq!(
                media.host_pcm.len(),
                (frame as usize + 1).min(MAX_PREHOST_PCM_FRAMES)
            );
        }
        // A missing packet must advance the actual native predictor even
        // before host attachment. SequenceTracker bounds this PLC to 3 frames.
        let payload = sender.encode(&voiced_pcm(41)).await.unwrap();
        expected.push(reference.decode_bits(vec![], 14, true).await.unwrap());
        expected.push(reference.decode(&payload).await.unwrap());
        let bytes = rtp_packet(96, 141, 45120, 777, &payload);
        ingress.lock().unwrap().push(
            id,
            &RtpPacket::parse(&bytes).unwrap(),
            std::time::Instant::now() - Duration::from_millis(50),
        );
        receive_ready(&mut media, &audio, prefix, None, &stats)
            .await
            .unwrap();
        assert_eq!(stats.lock().unwrap().rtp_packets_received, 41);
        assert_eq!(stats.lock().unwrap().audio_frames_to_host, 0);
        assert_eq!(stats.lock().unwrap().rtp_decode_errors, 0);
        assert_eq!(media.host_pcm.len(), MAX_PREHOST_PCM_FRAMES);
        let expected = &expected[expected.len() - MAX_PREHOST_PCM_FRAMES..];
        for (queued, reference) in media.host_pcm.iter().zip(expected) {
            assert_eq!(queued.as_slice(), reference.as_slice());
        }
        receive_ready(
            &mut media,
            &audio,
            prefix,
            Some(host.local_addr().unwrap()),
            &stats,
        )
        .await
        .unwrap();
        assert!(media.host_pcm.is_empty());
        let mut out = [0; 2048];
        for reference in expected {
            let (count, _) = tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut out))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&out[..prefix.len()], prefix);
            assert_eq!(&out[prefix.len()..count], reference.as_slice());
        }
        assert_eq!(stats.lock().unwrap().audio_frames_to_host, 32);
        queue_host_pcm(&mut media.host_pcm, vec![9; 640]);
        media.close().await.unwrap();
        assert!(
            media.host_pcm.is_empty(),
            "stop must zeroize this call's undelivered PCM"
        );
        assert!(NegotiationState::default().host_pcm.is_empty());
        sender.close().await.unwrap();
        reference.close().await.unwrap();
    }
}

#[tokio::test]
async fn offerless_ack_before_host_attachment_retains_actual_evs_and_wb_pcm_until_valid_token() {
    for payload_type in [96, 98] {
        let config = |ip: &str| TunnelConfiguration {
            ipv4_addresses: vec![ip.parse().unwrap()],
            ..Default::default()
        };
        let a = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
        let b = TunnelNetwork::new(&config("10.0.0.3")).unwrap();
        let (ab_tx, ab_rx) = mpsc::channel(256);
        let (ba_tx, ba_rx) = mpsc::channel(256);
        let (stop, stop_rx) = watch::channel(false);
        let (wa, ra) = watch::channel(0);
        let (wb, rb) = watch::channel(0);
        let (ca, sa) = UdpSocketRegistry::new(wa);
        let (cb, sb) = UdpSocketRegistry::new(wb);
        let client = tokio::spawn(a.run_managed(
            SocketSet::new(vec![]),
            ba_rx,
            ab_tx,
            stop_rx.clone(),
            ra,
            sa,
        ));
        let server =
            tokio::spawn(b.run_managed(SocketSet::new(vec![]), ab_rx, ba_tx, stop_rx, rb, sb));
        let generation = SessionGeneration::from_value(904).unwrap();
        let binding = CallBinding {
            generation,
            call_id: format!("ack-before-host-{payload_type}"),
            invite_cseq: 1,
        };
        let owner = Owner::new(generation, "10.0.0.2".parse().unwrap(), ca);
        let local = owner.clone().allocate(binding.clone()).await.unwrap();
        let offer = initial_dual_local_offer(local.ip, local.port);
        let codec = local_receive_formats(&offer)
            .unwrap()
            .into_iter()
            .find(|(pt, _)| *pt == payload_type)
            .unwrap()
            .1;
        owner
            .clone()
            .prepare_local_offer(binding.clone(), offer)
            .await
            .unwrap();
        let mut remote = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
        let mut sender = CodecWorker::new(codec).await.unwrap();
        let mut reference = CodecWorker::new(codec).await.unwrap();
        let destination = SocketAddr::new(local.ip, local.port);
        let bridge = SocketAddr::new("127.0.0.1".parse().unwrap(), local.audio_port);
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut invalid = b"invalid-token\0".to_vec();
        invalid.extend_from_slice(&voiced_pcm(0));
        host.send_to(&invalid, bridge).await.unwrap();
        let mut expected = vec![];
        for frame in 0..4 {
            let payload = sender.encode(&voiced_pcm(frame)).await.unwrap();
            expected.push(reference.decode(&payload).await.unwrap());
            remote
                .sender()
                .send_to(
                    rtp_packet(
                        payload_type,
                        100 + frame as u16,
                        32000 + frame as u32 * 320,
                        777,
                        &payload,
                    ),
                    destination,
                )
                .await
                .unwrap();
        }
        assert!(owner.clone().dtmf(binding.clone(), '#').await.is_err());
        owner
            .clone()
            .configure(
                binding.clone(),
                AudioNegotiation {
                    remote_ip: remote.local_addr().ip(),
                    remote_port: remote.local_addr().port(),
                    payload_type,
                    codec,
                    telephone_event_payload: Some(102),
                    telephone_event_mask: 0xffff,
                },
            )
            .await
            .unwrap();
        owner.clone().start(binding.clone(), false).await.unwrap();
        for _ in 0..400 {
            if owner.statistics(&binding).rtp_packets_received == 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(owner.statistics(&binding).rtp_packets_received, 4);
        assert_eq!(owner.statistics(&binding).audio_frames_to_host, 0);
        host.send_to(&invalid, bridge).await.unwrap();
        let mut output = [0; 2048];
        assert!(
            tokio::time::timeout(Duration::from_millis(20), host.recv_from(&mut output))
                .await
                .is_err(),
            "an invalid token cannot flush retained audio"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), remote.recv())
                .await
                .is_err(),
            "RTP decode, ACK and invalid host PCM must never generate voice TX"
        );
        let prefix = format!("{}\0", local.audio_token).into_bytes();
        host.send_to(&prefix, bridge).await.unwrap();
        for frame in &expected {
            let (count, source) =
                tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut output))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(source, bridge);
            assert_eq!(count, prefix.len() + 640);
            assert_eq!(&output[..prefix.len()], prefix.as_slice());
            assert_eq!(
                &output[prefix.len()..count],
                frame.as_slice(),
                "pre-host PCM must retain the native decoder's exact order"
            );
        }
        assert_eq!(owner.statistics(&binding).audio_frames_to_host, 4);
        let payload = sender.encode(&voiced_pcm(4)).await.unwrap();
        let expected_next = reference.decode(&payload).await.unwrap();
        remote
            .sender()
            .send_to(
                rtp_packet(payload_type, 104, 33280, 777, &payload),
                destination,
            )
            .await
            .unwrap();
        let (count, _) = tokio::time::timeout(Duration::from_secs(2), host.recv_from(&mut output))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&output[prefix.len()..count], expected_next.as_slice());
        assert_eq!(owner.statistics(&binding).rtp_packets_sent, 0);
        owner.clone().stop(binding.clone()).await.unwrap();
        assert!(owner.state.lock().await.is_none());

        // Stop another call while it still has undelivered PCM, then attach
        // the next allocation with the old token before its own valid token.
        let queued_binding = CallBinding {
            call_id: "stopped-prehost-queue".into(),
            ..binding.clone()
        };
        let queued = owner
            .clone()
            .allocate(queued_binding.clone())
            .await
            .unwrap();
        owner
            .clone()
            .configure(
                queued_binding.clone(),
                AudioNegotiation {
                    remote_ip: remote.local_addr().ip(),
                    remote_port: remote.local_addr().port(),
                    payload_type,
                    codec,
                    telephone_event_payload: None,
                    telephone_event_mask: 0,
                },
            )
            .await
            .unwrap();
        let payload = sender.encode(&voiced_pcm(5)).await.unwrap();
        remote
            .sender()
            .send_to(
                rtp_packet(payload_type, 200, 64000, 888, &payload),
                SocketAddr::new(queued.ip, queued.port),
            )
            .await
            .unwrap();
        for _ in 0..400 {
            if owner.statistics(&queued_binding).rtp_packets_received == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(owner.statistics(&queued_binding).rtp_packets_received, 1);
        assert_eq!(owner.statistics(&queued_binding).audio_frames_to_host, 0);
        owner.clone().stop(queued_binding).await.unwrap();
        let next_binding = CallBinding {
            call_id: "fresh-prehost-queue".into(),
            ..binding
        };
        let next = owner.clone().allocate(next_binding.clone()).await.unwrap();
        assert_ne!(next.audio_token, queued.audio_token);
        let next_bridge = SocketAddr::new("127.0.0.1".parse().unwrap(), next.audio_port);
        host.send_to(format!("{}\0", queued.audio_token).as_bytes(), next_bridge)
            .await
            .unwrap();
        host.send_to(format!("{}\0", next.audio_token).as_bytes(), next_bridge)
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), host.recv_from(&mut output))
                .await
                .is_err(),
            "stopped-call PCM cannot escape through the next token/socket"
        );
        assert_eq!(owner.statistics(&next_binding).audio_frames_to_host, 0);
        assert_eq!(owner.statistics(&next_binding).rtp_packets_sent, 0);
        owner.clone().stop(next_binding).await.unwrap();
        sender.close().await.unwrap();
        reference.close().await.unwrap();
        remote.close().await.unwrap();
        stop.send_replace(true);
        client.await.unwrap().unwrap();
        server.await.unwrap().unwrap();
    }
}
