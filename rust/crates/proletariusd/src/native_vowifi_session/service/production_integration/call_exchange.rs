//! Public voice API -> production Service/LiveDriver -> protected SIP actor,
//! real codec/RTP over outer ESP, host PCM bridge, history and actual teardown.
use super::{
    call_peer::{Command, Event, NegativeKind, RackCase},
    *,
};
use serde_json::Value;

pub(super) async fn http(
    fixture: &Fixture,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let body = public_call_body(method, path, body);
    let response = gateway::request(
        fixture,
        Request::builder()
            .method(method)
            .uri(path)
            .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
            .unwrap(),
    )
    .await;
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
pub(super) async fn state(fixture: &Fixture, wanted: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (status, value) =
                http(fixture, "GET", "/api/reader/vowifi/calls/status", json!({})).await;
            assert_eq!(status, StatusCode::OK, "{value}");
            if value["call"]["state"] == wanted {
                return value;
            }
            assert_ne!(value["call"]["state"], "failed", "{value}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("public call state deadline")
}
// The host can receive UDP before deliver() publishes its successful-send
// counter. SIP phase transitions are independent, so wait for the public
// snapshot to account for these exact frames on the same call. Extra packets
// fail immediately; missing accounting has the existing media deadline.
async fn media_statistics(fixture: &Fixture, binding: &Value, expected: [u64; 4]) -> Value {
    media_statistics_on(
        &fixture.sms_state(),
        "/api/reader/vowifi/calls/status",
        binding,
        expected,
    )
    .await
}
async fn media_statistics_on(
    server: &crate::ServerState,
    path: &str,
    binding: &Value,
    expected: [u64; 4],
) -> Value {
    let mut last_stats = Value::Null;
    let completed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, stats) =
                super::sms_exchange::sms_http(server, "GET", path, json!({})).await;
            assert_eq!(status, StatusCode::OK, "{stats}");
            assert_eq!(stats["generation"], binding["generation"], "{stats}");
            for field in [
                "state",
                "call_id",
                "started_at",
                "audio_port",
                "audio_token",
            ] {
                assert_eq!(
                    stats["call"][field], binding["call"][field],
                    "preserve {field}: {stats}"
                );
            }
            let mut complete = true;
            for (field, target) in [
                "rtp_packets_sent",
                "rtp_packets_received",
                "rtp_decode_errors",
                "audio_frames_to_host",
            ]
            .into_iter()
            .zip(expected)
            {
                let actual = stats["call"][field].as_u64().expect("public media counter");
                assert!(actual <= target, "unexpected extra {field}: {stats}");
                complete &= actual == target;
            }
            last_stats = stats;
            if complete {
                return last_stats.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    completed
        .unwrap_or_else(|_| panic!("public media statistics completion deadline: {last_stats}"))
}

pub(super) async fn media(
    fixture: &Fixture,
    client: &mut call_peer::Client,
    active: &Value,
    codec: &str,
) {
    media_on(
        &fixture.sms_state(),
        "/api/reader/vowifi/calls/status",
        client,
        active,
        codec,
    )
    .await;
}
pub(super) async fn media_on(
    server: &crate::ServerState,
    path: &str,
    client: &mut call_peer::Client,
    active: &Value,
    codec: &str,
) {
    use tokio::net::UdpSocket;
    assert_eq!(active["call"]["codec"], codec);
    assert_eq!(active["call"]["audio_sample_rate"], 16000);
    assert_eq!(active["call"]["media_ready"], true);
    let (_, before) = super::sms_exchange::sms_http(server, "GET", path, json!({})).await;
    let sent = before["call"]["rtp_packets_sent"].as_u64().unwrap();
    let received = before["call"]["rtp_packets_received"].as_u64().unwrap();
    let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = active["call"]["audio_port"].as_u64().unwrap() as u16;
    let bridge = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let prefix = format!("{}\0", active["call"]["audio_token"].as_str().unwrap()).into_bytes();
    // Unauthorized PCM cannot produce a tunneled RTP packet.
    let mut bad = b"incorrect-token\0".to_vec();
    bad.extend([0u8; 640]);
    host.send_to(&bad, bridge).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let (_, unchanged) = super::sms_exchange::sms_http(server, "GET", path, json!({})).await;
    assert_eq!(unchanged["call"]["rtp_packets_sent"], sent);
    // Exercise actual speech encode/decode with non-silent signed 16-bit PCM.
    let mut pcm = prefix.clone();
    for sample in 0..320 {
        pcm.extend((((sample % 40) as i16 - 20) * 500).to_le_bytes());
    }
    // Several frames account for the real codec's algorithmic startup delay.
    let mut speech = false;
    for _ in 0..4 {
        host.send_to(&pcm, bridge).await.unwrap();
        let Event::Audio {
            codec: observed,
            payload,
            ..
        } = client.next().await
        else {
            panic!("missing actual RTP speech")
        };
        assert_eq!(observed, codec);
        assert!(payload > 20);
        let mut decoded = [0u8; 2048];
        let (count, source) =
            tokio::time::timeout(Duration::from_secs(3), host.recv_from(&mut decoded))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(source, bridge);
        assert_eq!(count, prefix.len() + 640);
        assert_eq!(&decoded[..prefix.len()], prefix.as_slice());
        speech |= decoded[prefix.len()..count].iter().any(|v| *v != 0);
    }
    assert!(
        speech,
        "codec produced only silence across four speech frames"
    );
    let stats = media_statistics_on(
        server,
        path,
        active,
        [sent + 4, received + 4, 0, received + 4],
    )
    .await;
    assert_eq!(stats["call"]["rtp_packets_sent"], sent + 4);
    assert_eq!(stats["call"]["rtp_packets_received"], received + 4);
    assert_eq!(stats["call"]["rtp_decode_errors"], 0);
    assert_eq!(stats["call"]["audio_frames_to_host"], received + 4);
}

async fn amr_wb_cmr_on_the_live_dialog(
    fixture: &Fixture,
    client: &mut call_peer::Client,
    active: &Value,
) {
    let host = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = active["call"]["audio_port"].as_u64().unwrap() as u16;
    let bridge = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let prefix = format!("{}\0", active["call"]["audio_token"].as_str().unwrap()).into_bytes();
    let mut pcm = prefix.clone();
    for sample in 0..320 {
        pcm.extend((((sample % 40) as i16 - 20) * 500).to_le_bytes());
    }
    let (_, before) = http(fixture, "GET", "/api/reader/vowifi/calls/status", json!({})).await;
    let sent = before["call"]["rtp_packets_sent"].as_u64().unwrap();
    let received = before["call"]["rtp_packets_received"].as_u64().unwrap();
    let mut previous = None;
    let mut current_mode = 8;
    let mut frames = 0;
    // One CMR changes the existing encoder; cancellation, all reserved values
    // and every unsupported speech mode must leave mode2 in place. A final
    // CMR8 proves that this same live dialog can return to its original mode.
    for (raw_cmr, expected_mode) in std::iter::once((2, 2))
        .chain(std::iter::once((15, 2)))
        .chain((9..=14).map(|cmr| (cmr, 2)))
        .chain([0, 1, 3, 4, 5, 6, 7].map(|cmr| (cmr, 2)))
        .chain([(8, 8), (15, 8)])
    {
        client
            .commands
            .send(Command::AmrWbCmr(raw_cmr))
            .await
            .unwrap();
        assert_eq!(client.next().await, Event::CmrArmed(raw_cmr));
        for expected in [current_mode, expected_mode] {
            host.send_to(&pcm, bridge).await.unwrap();
            let Event::Audio {
                codec,
                payload,
                mode,
                ssrc,
                sequence,
                timestamp,
                ..
            } = client.next().await
            else {
                panic!("missing RTP after CMR {raw_cmr}")
            };
            assert_eq!(codec, "AMR-WB");
            assert_eq!(mode, Some(expected), "CMR {raw_cmr} next-frame mode");
            assert_eq!(
                payload,
                2 + proletarius_call::amr_wb::frame_bits(expected)
                    .unwrap()
                    .div_ceil(8)
            );
            if let Some((old_ssrc, old_sequence, old_timestamp)) = previous {
                assert_eq!(ssrc, old_ssrc, "CMR must not replace RTP ownership");
                assert_eq!(sequence, u16::wrapping_add(old_sequence, 1));
                assert_eq!(timestamp, u32::wrapping_add(old_timestamp, 320));
            }
            previous = Some((ssrc, sequence, timestamp));
            // A decoded response is the synchronization barrier: the CMR went
            // carrier RTP -> real ESP -> ordered RTP -> production decoder.
            // Only then is the next host frame submitted to the same encoder.
            let mut decoded = [0u8; 2048];
            let (count, source) =
                tokio::time::timeout(Duration::from_secs(3), host.recv_from(&mut decoded))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(source, bridge);
            assert_eq!(count, prefix.len() + 640);
            assert_eq!(&decoded[..prefix.len()], prefix.as_slice());
            frames += 1;
        }
        current_mode = expected_mode;
    }
    let after = media_statistics(
        fixture,
        active,
        [sent + frames, received + frames, 0, received + frames],
    )
    .await;
    assert_eq!(after["call"]["rtp_packets_sent"], sent + frames);
    assert_eq!(after["call"]["rtp_packets_received"], received + frames);
    assert_eq!(after["call"]["rtp_decode_errors"], 0);
    assert_eq!(after["call"]["audio_frames_to_host"], received + frames);
}
async fn incoming_media_before_initial_ack(
    fixture: &Fixture,
    client: &mut call_peer::Client,
) -> Value {
    let connecting = state(fixture, "connecting").await;
    assert_eq!(connecting["call"]["codec"], "AMR-WB");
    assert_eq!(connecting["call"]["media_ready"], true);
    assert_eq!(connecting["call"]["early_media"], true);
    let host = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let bridge = std::net::SocketAddr::from((
        [127, 0, 0, 1],
        connecting["call"]["audio_port"].as_u64().unwrap() as u16,
    ));
    let prefix = format!("{}\0", connecting["call"]["audio_token"].as_str().unwrap()).into_bytes();
    let mut pcm = prefix.clone();
    for sample in 0..320 {
        pcm.extend((((sample % 40) as i16 - 20) * 500).to_le_bytes());
    }
    // Even a complete authenticated microphone frame may only attach the host
    // while the initial200 is awaiting ACK; it must produce no uplink RTP.
    host.send_to(&pcm, bridge).await.unwrap();
    let mut retransmissions = 0;
    while retransmissions < 2 {
        assert_eq!(client.next().await, Event::AnswerRetransmitted);
        retransmissions += 1;
    }
    let mut previous_downlink = None;
    let mut speech = false;
    for _ in 0..4 {
        client
            .commands
            .send(Command::SendIncomingAudio)
            .await
            .unwrap();
        let (ssrc, sequence, timestamp, payload) = loop {
            match client.next().await {
                Event::AnswerRetransmitted => retransmissions += 1,
                Event::CarrierAudio {
                    ssrc,
                    sequence,
                    timestamp,
                    payload,
                } => break (ssrc, sequence, timestamp, payload),
                event => panic!("unexpected pre-ACK media event: {event:?}"),
            }
        };
        assert!(payload > 20, "real AMR-WB speech payload");
        if let Some((old_ssrc, old_sequence, old_timestamp)) = previous_downlink {
            assert_eq!(ssrc, old_ssrc);
            assert_eq!(sequence, u16::wrapping_add(old_sequence, 1));
            assert_eq!(timestamp, u32::wrapping_add(old_timestamp, 320));
        }
        previous_downlink = Some((ssrc, sequence, timestamp));
        let mut decoded = [0u8; 2048];
        let (count, source) =
            tokio::time::timeout(Duration::from_secs(3), host.recv_from(&mut decoded))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(source, bridge);
        assert_eq!(count, prefix.len() + 640);
        assert_eq!(&decoded[..prefix.len()], prefix.as_slice());
        speech |= decoded[prefix.len()..count].iter().any(|value| *value != 0);
        // Actual host PCM completion is the only next-packet barrier.
    }
    assert!(
        speech,
        "real downlink speech must reach the host before any initial ACK"
    );
    let pre_ack = media_statistics(fixture, &connecting, [0, 4, 0, 4]).await;
    assert_eq!(pre_ack["call"]["early_media"], true);
    assert_eq!(pre_ack["call"]["rtp_packets_sent"], 0);
    assert_eq!(pre_ack["call"]["rtp_packets_received"], 4);
    assert_eq!(pre_ack["call"]["audio_frames_to_host"], 4);
    assert_eq!(pre_ack["call"]["rtp_decode_errors"], 0);
    assert!(retransmissions >= 2);
    let mut initial_started_at = None;
    let mut previous_uplink = None;
    let mut final_active = None;
    for duplicate in [false, true] {
        client.commands.send(Command::AckIncoming).await.unwrap();
        loop {
            match client.next().await {
                Event::IncomingAckSent => break,
                Event::AnswerRetransmitted => retransmissions += 1,
                event => panic!("unexpected initial ACK event: {event:?}"),
            }
        }
        let active = state(fixture, "active").await;
        assert_eq!(active["call"]["early_media"], false);
        assert_eq!(active["call"]["media_ready"], true);
        assert_eq!(
            active["call"]["audio_port"],
            connecting["call"]["audio_port"]
        );
        assert_eq!(
            active["call"]["audio_token"],
            connecting["call"]["audio_token"]
        );
        let started_at = active["call"]["started_at"]
            .as_str()
            .expect("ACK activates a timed call");
        assert!(!started_at.is_empty());
        if let Some(original) = &initial_started_at {
            assert_eq!(started_at, original);
        } else {
            initial_started_at = Some(started_at.to_owned());
        }
        let mut carrier_speech = false;
        let mut host_speech = false;
        for _ in 0..4 {
            host.send_to(&pcm, bridge).await.unwrap();
            let Event::Audio {
                codec,
                payload,
                ssrc,
                sequence,
                timestamp,
                peer_speech,
                ..
            } = client.next().await
            else {
                panic!("missing actual speech after initial/duplicate ACK")
            };
            assert_eq!(codec, "AMR-WB");
            assert!(payload > 20);
            if let Some((old_ssrc, old_sequence, old_timestamp)) = previous_uplink {
                assert_eq!(ssrc, old_ssrc, "duplicate ACK cannot replace RTP ownership");
                assert_eq!(sequence, u16::wrapping_add(old_sequence, 1));
                assert_eq!(timestamp, u32::wrapping_add(old_timestamp, 320));
            }
            previous_uplink = Some((ssrc, sequence, timestamp));
            carrier_speech |= peer_speech;
            let mut decoded = [0u8; 2048];
            let (count, source) =
                tokio::time::timeout(Duration::from_secs(3), host.recv_from(&mut decoded))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(source, bridge);
            assert_eq!(count, prefix.len() + 640);
            assert_eq!(&decoded[..prefix.len()], prefix.as_slice());
            host_speech |= decoded[prefix.len()..count].iter().any(|value| *value != 0);
        }
        assert!(carrier_speech && host_speech);
        let frames = if duplicate { 8 } else { 4 };
        let stats = media_statistics(fixture, &active, [frames, 4 + frames, 0, 4 + frames]).await;
        assert_eq!(stats["call"]["rtp_packets_sent"], frames);
        assert_eq!(stats["call"]["rtp_packets_received"], 4 + frames);
        assert_eq!(stats["call"]["audio_frames_to_host"], 4 + frames);
        assert_eq!(stats["call"]["rtp_decode_errors"], 0);
        assert_eq!(
            stats["call"]["started_at"],
            initial_started_at.as_ref().unwrap().as_str()
        );
        final_active = Some(stats);
    }
    final_active.unwrap()
}

async fn replay_accepted_transactions(
    client: &mut call_peer::Client,
    transactions: usize,
    previous_dialog: bool,
    active: bool,
) {
    client
        .commands
        .send(Command::ReplayAccepted { previous_dialog })
        .await
        .unwrap();
    let Event::AcceptedReplayChecked {
        transactions: observed,
        previous_dialog: observed_previous,
        status,
    } = client.next().await
    else {
        panic!("missing same-TCP OPTIONS plus calls-owner INFO Accepted replay barrier")
    };
    assert_eq!(observed, transactions);
    assert_eq!(observed_previous, previous_dialog);
    if active {
        assert_eq!(
            status, 200,
            "old transactions must leave the current dialog usable"
        );
    } else {
        assert!(matches!(status, 200 | 481));
    }
}

async fn renegotiate_on_the_live_dialog(
    fixture: &Fixture,
    client: &mut call_peer::Client,
    active: &Value,
    incoming: bool,
) {
    use proletarius_call::sdp::{AmrWbModePolicy, AudioCodec};
    let host = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let bridge = std::net::SocketAddr::from((
        [127, 0, 0, 1],
        active["call"]["audio_port"].as_u64().unwrap() as u16,
    ));
    let prefix = format!("{}\0", active["call"]["audio_token"].as_str().unwrap()).into_bytes();
    let mut pcm = prefix.clone();
    for sample in 0..320 {
        pcm.extend((((sample % 40) as i16 - 20) * 500).to_le_bytes());
    }
    let started_at = active["call"]["started_at"]
        .as_str()
        .expect("call start time");
    assert!(!started_at.is_empty());
    let wb = AudioCodec::AmrWb {
        octet_aligned: true,
        mode: 8,
        policy: AmrWbModePolicy::new((1 << 2) | (1 << 8), 1, false).unwrap(),
    };
    let narrow = AudioCodec::AmrWb {
        octet_aligned: true,
        mode: 2,
        policy: AmrWbModePolicy::new(1 << 2, 2, true).unwrap(),
    };
    let be = AudioCodec::AmrWb {
        octet_aligned: false,
        mode: 2,
        policy: AmrWbModePolicy::single(2).unwrap(),
    };
    let evs = AudioCodec::Evs { header_full: false };
    // Use the same retained PT mappings in both dialog directions. Framing
    // changes receive distinct PTs so old/new RTP is unambiguous in flight.
    let mut phases = if incoming {
        vec![
            (wb, 98),
            (evs, 96),
            (wb, 98),
            (narrow, 98),
            (be, 110),
            (evs, 96),
            (wb, 98),
        ]
    } else {
        vec![
            (evs, 96),
            (wb, 98),
            (narrow, 98),
            (be, 110),
            (evs, 96),
            (AudioCodec::Evs { header_full: true }, 111),
            (evs, 96),
        ]
    };
    let invite_phases = phases.len();
    // Two UPDATE exchanges return each live dialog to its prior codec. Unlike
    // re-INVITE, neither 200 is followed by ACK before the next actual frame.
    phases.extend(if incoming {
        [(evs, 96), (wb, 98)]
    } else {
        [(wb, 98), (evs, 96)]
    });
    let (_, before) = http(fixture, "GET", "/api/reader/vowifi/calls/status", json!({})).await;
    let sent = before["call"]["rtp_packets_sent"].as_u64().unwrap();
    let received = before["call"]["rtp_packets_received"].as_u64().unwrap();
    let mut previous = None;
    let mut frames = 0;
    for (phase, (codec, payload_type)) in phases.into_iter().enumerate() {
        let name = match codec {
            AudioCodec::Evs { .. } => "EVS",
            AudioCodec::AmrWb { .. } => "AMR-WB",
        };
        if phase != 0 {
            let update = phase >= invite_phases;
            let command = if update {
                Command::Update {
                    codec,
                    payload: payload_type,
                }
            } else {
                Command::Renegotiate {
                    codec,
                    payload: payload_type,
                    period: if codec == narrow { 2 } else { 1 },
                    neighbor: codec == narrow,
                }
            };
            client.commands.send(command).await.unwrap();
            assert_eq!(
                client.next().await,
                if update {
                    Event::Updated
                } else {
                    Event::Renegotiated
                }
            );
            // The carrier has written the matching ACK. Drain that stream,
            // replay every earlier accepted INVITE/ACK, then require another
            // OPTIONS plus owner INFO response. No cached 200 may hide a missing ledger row.
            replay_accepted_transactions(client, phase.min(invite_phases - 1), false, true).await;
            if incoming && phase == 2 {
                // A new incoming dialog is now carrying actual speech. Old
                // outgoing Accepted transactions must remain independent of it.
                replay_accepted_transactions(client, invite_phases, true, true).await;
                replay_completed_transactions(client, 5, true, true).await;
            }
        }
        let snapshot = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = state(fixture, "active").await;
                if snapshot["call"]["codec"] == name {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("accepted SDP exchange must publish the actual negotiated codec");
        assert_eq!(snapshot["call"]["started_at"], started_at);
        assert_eq!(snapshot["call"]["audio_port"], active["call"]["audio_port"]);
        assert_eq!(
            snapshot["call"]["audio_token"],
            active["call"]["audio_token"]
        );
        assert_eq!(snapshot["call"]["media_ready"], true);
        let mut peer_speech = false;
        let mut host_speech = false;
        for _ in 0..4 {
            host.send_to(&pcm, bridge).await.unwrap();
            let Event::Audio {
                codec: observed,
                payload,
                mode,
                ssrc,
                sequence,
                timestamp,
                peer_speech: decoded,
            } = client.next().await
            else {
                panic!("missing actual speech in renegotiation phase {phase}")
            };
            assert_eq!(observed, name);
            assert!(payload > 20);
            if let AudioCodec::AmrWb { policy, .. } = codec {
                assert!(policy.contains(mode.expect("actual AMR-WB FT")));
            }
            if let Some((old_ssrc, old_sequence, old_timestamp)) = previous {
                assert_eq!(ssrc, old_ssrc, "codec switch must preserve RTP ownership");
                assert_eq!(sequence, u16::wrapping_add(old_sequence, 1));
                assert_eq!(timestamp, u32::wrapping_add(old_timestamp, 320));
            }
            previous = Some((ssrc, sequence, timestamp));
            peer_speech |= decoded;
            // This actual host receive is the next-send barrier. No timing
            // delay substitutes for carrier decode -> ESP -> production
            // decode -> authenticated host PCM completion.
            let mut decoded = [0u8; 2048];
            let (count, source) =
                tokio::time::timeout(Duration::from_secs(3), host.recv_from(&mut decoded))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(source, bridge);
            assert_eq!(count, prefix.len() + 640);
            assert_eq!(&decoded[..prefix.len()], prefix.as_slice());
            host_speech |= decoded[prefix.len()..count]
                .iter()
                .any(|sample| *sample != 0);
            frames += 1;
        }
        assert!(
            peer_speech && host_speech,
            "both real decoders must produce speech in phase {phase}"
        );
    }
    // A new offer cannot rename the previously advertised EVS PT96 to AMR-WB.
    // The accepted mapping and active PCM bridge must survive rejection.
    for update in [false, true] {
        let command = if update {
            Command::Update {
                codec: wb,
                payload: 96,
            }
        } else {
            Command::Renegotiate {
                codec: wb,
                payload: 96,
                period: 1,
                neighbor: false,
            }
        };
        client.commands.send(command).await.unwrap();
        assert_eq!(
            client.next().await,
            if update {
                Event::UpdateRejected(488)
            } else {
                Event::RenegotiationRejected(488)
            }
        );
        // Rejected transactions cannot overwrite any of the earlier Accepted
        // identities. The next voiced frame must still use the latest codec.
        replay_accepted_transactions(client, invite_phases - 1, false, true).await;
        // A rejected SDP must leave a usable real worker, not only a snapshot.
        host.send_to(&pcm, bridge).await.unwrap();
        let Event::Audio {
            codec,
            ssrc,
            sequence,
            timestamp,
            ..
        } = client.next().await
        else {
            panic!("rejection lost the accepted media worker")
        };
        assert_eq!(codec, if incoming { "AMR-WB" } else { "EVS" });
        let (old_ssrc, old_sequence, old_timestamp) = previous.unwrap();
        assert_eq!(ssrc, old_ssrc);
        assert_eq!(sequence, old_sequence.wrapping_add(1));
        assert_eq!(timestamp, old_timestamp.wrapping_add(320));
        let mut decoded = [0u8; 2048];
        let (count, source) =
            tokio::time::timeout(Duration::from_secs(3), host.recv_from(&mut decoded))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(source, bridge);
        assert_eq!(count, prefix.len() + 640);
        assert_eq!(&decoded[..prefix.len()], prefix.as_slice());
        previous = Some((ssrc, sequence, timestamp));
        frames += 1;
    }
    let after = media_statistics(
        fixture,
        active,
        [sent + frames, received + frames, 0, received + frames],
    )
    .await;
    assert_eq!(
        after["call"]["codec"],
        if incoming { "AMR-WB" } else { "EVS" }
    );
    assert_eq!(after["call"]["started_at"], started_at);
    assert_eq!(after["call"]["audio_port"], active["call"]["audio_port"]);
    assert_eq!(after["call"]["audio_token"], active["call"]["audio_token"]);
    assert_eq!(after["call"]["rtp_packets_sent"], sent + frames);
    assert_eq!(after["call"]["rtp_packets_received"], received + frames);
    assert_eq!(after["call"]["rtp_decode_errors"], 0);
    assert_eq!(after["call"]["audio_frames_to_host"], received + frames);
}

async fn completed_event(client: &mut call_peer::Client) -> Event {
    loop {
        match client.next().await {
            Event::ReinviteRetransmitted => {}
            event => return event,
        }
    }
}
async fn replay_completed_transactions(
    client: &mut call_peer::Client,
    transactions: usize,
    previous_dialog: bool,
    active: bool,
) {
    client
        .commands
        .send(Command::ReplayCompleted { previous_dialog })
        .await
        .unwrap();
    let Event::CompletedReplayed {
        transactions: observed,
        previous_dialog: observed_previous,
        status,
    } = completed_event(client).await
    else {
        panic!("Completed final replay requires socket OPTIONS plus owner INFO barriers")
    };
    assert_eq!(observed, transactions);
    assert_eq!(observed_previous, previous_dialog);
    if active {
        assert_eq!(status, 200);
    } else {
        assert!(matches!(status, 200 | 481));
    }
}
async fn completed_on_the_live_dialog(
    fixture: &Fixture,
    client: &mut call_peer::Client,
    active: &Value,
    name: &str,
) {
    let host = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let bridge = std::net::SocketAddr::from((
        [127, 0, 0, 1],
        active["call"]["audio_port"].as_u64().unwrap() as u16,
    ));
    let prefix = format!("{}\0", active["call"]["audio_token"].as_str().unwrap()).into_bytes();
    let mut pcm = prefix.clone();
    for sample in 0..320 {
        pcm.extend((((sample % 40) as i16 - 20) * 500).to_le_bytes());
    }
    let (_, before) = http(fixture, "GET", "/api/reader/vowifi/calls/status", json!({})).await;
    let sent = before["call"]["rtp_packets_sent"].as_u64().unwrap();
    let received = before["call"]["rtp_packets_received"].as_u64().unwrap();
    let host_frames = before["call"]["audio_frames_to_host"].as_u64().unwrap();
    client.commands.send(Command::HoldReinvite).await.unwrap();
    assert_eq!(client.next().await, Event::ReinvitePendingAck);
    client
        .commands
        .send(Command::NegativeInvite {
            kind: NegativeKind::Pending,
        })
        .await
        .unwrap();
    assert_eq!(completed_event(client).await, Event::NegativeFinal(491));
    client
        .commands
        .send(Command::NegativeInvite {
            kind: NegativeKind::Collision,
        })
        .await
        .unwrap();
    assert_eq!(completed_event(client).await, Event::NegativeFinal(491));
    for _ in 0..2 {
        client
            .commands
            .send(Command::AckCompletedCollision)
            .await
            .unwrap();
        assert_eq!(completed_event(client).await, Event::NegativeAckSent);
    }
    client.commands.send(Command::ProbeDialog).await.unwrap();
    assert_eq!(completed_event(client).await, Event::DialogProbed(200));
    // This wait starts after the wrong-branch ACK has crossed the calls-owner INFO
    // processing barrier. Pre-ACK queued retransmissions cannot satisfy it.
    for _ in 0..2 {
        assert_eq!(client.next().await, Event::ReinviteRetransmitted);
    }
    let mut previous = None;
    let mut frames = 0;
    for phase in 0..6 {
        if phase == 1 {
            client
                .commands
                .send(Command::AckHeldReinvite)
                .await
                .unwrap();
            assert_eq!(completed_event(client).await, Event::ReinviteAckSent);
            client.commands.send(Command::ProbeDialog).await.unwrap();
            assert_eq!(client.next().await, Event::DialogProbed(200));
            // The outstanding exchange has now unlocked. Its old491 must
            // remain491 instead of admitting the previously rejected offer.
            replay_completed_transactions(client, 1, false, true).await;
        } else if phase >= 2 {
            let (kind, expected) = [
                (NegativeKind::BadContact, 400),
                (NegativeKind::WrongContentType, 415),
                (NegativeKind::ShortTimer, 422),
                (NegativeKind::BadCodec, 488),
            ][phase - 2];
            client
                .commands
                .send(Command::NegativeInvite { kind })
                .await
                .unwrap();
            assert_eq!(client.next().await, Event::NegativeFinal(expected));
            // Each later final is interleaved with every earlier unACKed
            // Completed transaction, so a one-entry cache cannot pass.
            replay_completed_transactions(client, phase, false, true).await;
        }
        let mut peer_speech = false;
        let mut host_speech = false;
        for _ in 0..4 {
            host.send_to(&pcm, bridge).await.unwrap();
            let Event::Audio {
                codec,
                payload,
                ssrc,
                sequence,
                timestamp,
                peer_speech: speech,
                ..
            } = completed_event(client).await
            else {
                panic!("Completed final disturbed real speech phase {phase}")
            };
            assert_eq!(codec, name);
            assert!(payload > 20);
            if let Some((old_ssrc, old_sequence, old_timestamp)) = previous {
                assert_eq!(ssrc, old_ssrc);
                assert_eq!(sequence, u16::wrapping_add(old_sequence, 1));
                assert_eq!(timestamp, u32::wrapping_add(old_timestamp, 320));
            }
            previous = Some((ssrc, sequence, timestamp));
            peer_speech |= speech;
            host_speech |= offerless_host_pcm(&host, bridge, &prefix).await;
            frames += 1;
        }
        assert!(peer_speech && host_speech);
        let current = media_statistics(
            fixture,
            &before,
            [sent + frames, received + frames, 0, host_frames + frames],
        )
        .await;
        for key in [
            "call_id",
            "audio_port",
            "audio_token",
            "codec",
            "started_at",
        ] {
            assert_eq!(
                current["call"][key], before["call"][key],
                "Completed/negativeACK changed {key}"
            );
        }
        assert_eq!(current["call"]["media_ready"], true);
        assert_eq!(current["call"]["rtp_packets_sent"], sent + frames);
        assert_eq!(current["call"]["rtp_packets_received"], received + frames);
        assert_eq!(
            current["call"]["audio_frames_to_host"],
            host_frames + frames
        );
        assert_eq!(current["call"]["rtp_decode_errors"], 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_calls_use_production_ike_sip_real_evs_amr_wb_rtp_pcm_and_history() {
    if std::env::var_os("SWITCH_PRODUCTION_492_CALL_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/production-492-call-config-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier-overrides.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output=tokio::task::spawn_blocking(move||std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","native_vowifi_session::service::production_integration::call_exchange::public_calls_use_production_ike_sip_real_evs_amr_wb_rtp_pcm_and_history","--nocapture"])
            .env("SWITCH_PRODUCTION_492_CALL_CHILD","1").env("PROLETARIUS_VOWIFI_CONFIG",config).output().unwrap()).await.unwrap();
        assert!(
            output.status.success(),
            "child production voice fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(60), scenario())
        .await
        .expect("complete production voice deadline");
}
async fn retired_media(fixture: &Fixture, active: &Value) {
    let ended = state(fixture, "ended").await;
    assert_eq!(ended["call"]["media_ready"], false);
    assert!(ended["call"].get("audio_token").is_none());
    assert!(ended["call"].get("audio_port").is_none());
    let port = active["call"]["audio_port"].as_u64().unwrap() as u16;
    let reclaimed = tokio::net::UdpSocket::bind(("127.0.0.1", port))
        .await
        .expect("exact PCM socket must be retired before ended is published");
    drop(reclaimed);
}
async fn offerless_host_pcm(
    host: &tokio::net::UdpSocket,
    bridge: std::net::SocketAddr,
    prefix: &[u8],
) -> bool {
    let mut decoded = [0u8; 2048];
    let (count, source) =
        tokio::time::timeout(Duration::from_secs(3), host.recv_from(&mut decoded))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(source, bridge);
    assert_eq!(count, prefix.len() + 640);
    assert_eq!(&decoded[..prefix.len()], prefix);
    decoded[prefix.len()..count]
        .iter()
        .any(|sample| *sample != 0)
}
async fn offerless_incoming_call(
    fixture: &Fixture,
    client: &mut call_peer::Client,
    generation: u64,
    amr_wb: bool,
    replay_previous_ack: bool,
) {
    let name = if amr_wb { "AMR-WB" } else { "EVS" };
    let (_, before) = http(fixture, "GET", "/api/reader/vowifi/calls/status", json!({})).await;
    client
        .commands
        .send(Command::IncomingOfferless { amr_wb })
        .await
        .unwrap();
    assert_eq!(client.next().await, Event::OfferlessTrying);
    assert_eq!(client.next().await, Event::Ringing);
    let incoming = state(fixture, "incoming").await;
    assert_eq!(incoming["call"]["direction"], "incoming");
    assert_ne!(
        incoming["call"]["call_id"], before["call"]["call_id"],
        "each initial offerless INVITE owns a new dialog"
    );
    assert_eq!(incoming["call"]["media_ready"], false);
    let (status, answer) = http(
        fixture,
        "POST",
        "/api/reader/vowifi/calls/answer",
        json!({"generation":generation}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let Event::OfferlessAnswerPendingAck { codec, payload } = client.next().await else {
        panic!("offerless initial200 must contain a usable retained local offer")
    };
    assert_eq!(codec, name);
    assert!(payload >= 96);
    let connecting = state(fixture, "connecting").await;
    assert_eq!(connecting["call"]["media_ready"], false);
    assert_eq!(connecting["call"]["early_media"], false);
    let host = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let bridge = std::net::SocketAddr::from((
        [127, 0, 0, 1],
        connecting["call"]["audio_port"].as_u64().unwrap() as u16,
    ));
    let prefix = format!("{}\0", connecting["call"]["audio_token"].as_str().unwrap()).into_bytes();
    let mut pcm = prefix.clone();
    for sample in 0..320 {
        pcm.extend((((sample % 40) as i16 - 20) * 500).to_le_bytes());
    }
    // EVS attaches before ACK without permission to transmit. The second,
    // AMR-WB call deliberately lets ACK/configure/decode beat host attachment.
    if !amr_wb {
        host.send_to(&pcm, bridge).await.unwrap();
    }
    let (dtmf_status, _) = http(
        fixture,
        "POST",
        "/api/reader/vowifi/calls/dtmf",
        json!({"generation":generation,"digit":"#"}),
    )
    .await;
    assert_eq!(dtmf_status, StatusCode::BAD_GATEWAY);
    let mut retries = 0;
    while retries < 2 {
        assert_eq!(client.next().await, Event::AnswerRetransmitted);
        retries += 1;
    }
    client
        .commands
        .send(Command::UnassociatedPrack)
        .await
        .unwrap();
    loop {
        match client.next().await {
            Event::UnassociatedPrackRejected => break,
            Event::AnswerRetransmitted => retries += 1,
            event => panic!("unexpected unassociated PRACK outcome: {event:?}"),
        }
    }
    let mut previous_downlink = None;
    for _ in 0..4 {
        client
            .commands
            .send(Command::SendIncomingAudio)
            .await
            .unwrap();
        let (ssrc, sequence, timestamp) = loop {
            match client.next().await {
                Event::AnswerRetransmitted => retries += 1,
                Event::CarrierAudio {
                    payload,
                    ssrc,
                    sequence,
                    timestamp,
                } => {
                    assert!(
                        payload > 20,
                        "pre-answer traffic must be actual encoded speech"
                    );
                    break (ssrc, sequence, timestamp);
                }
                event => panic!("unexpected offerless pre-ACK event: {event:?}"),
            }
        };
        if let Some((old_ssrc, old_sequence, old_timestamp)) = previous_downlink {
            assert_eq!(ssrc, old_ssrc);
            assert_eq!(sequence, u16::wrapping_add(old_sequence, 1));
            assert_eq!(timestamp, u32::wrapping_add(old_timestamp, 320));
        }
        previous_downlink = Some((ssrc, sequence, timestamp));
    }
    let mut decoded = [0u8; 2048];
    assert!(
        tokio::time::timeout(Duration::from_millis(80), host.recv_from(&mut decoded))
            .await
            .is_err(),
        "an unanswered local offer must not decode pre-answer packets"
    );
    let waiting = state(fixture, "connecting").await;
    assert_eq!(waiting["call"]["rtp_packets_sent"], 0);
    assert_eq!(waiting["call"]["rtp_packets_received"], 0);
    assert_eq!(waiting["call"]["audio_frames_to_host"], 0);
    assert_eq!(waiting["call"]["rtp_decode_errors"], 0);
    client.commands.send(Command::AckIncoming).await.unwrap();
    loop {
        match client.next().await {
            Event::IncomingAckSent => break,
            Event::AnswerRetransmitted => retries += 1,
            event => panic!("unexpected offerless ACK event: {event:?}"),
        }
    }
    let active = state(fixture, "active").await;
    assert_eq!(active["call"]["codec"], name);
    assert_eq!(active["call"]["media_ready"], true);
    assert_eq!(active["call"]["early_media"], false);
    assert_eq!(
        active["call"]["audio_port"],
        connecting["call"]["audio_port"]
    );
    assert_eq!(
        active["call"]["audio_token"],
        connecting["call"]["audio_token"]
    );
    let started_at = active["call"]["started_at"].as_str().unwrap();
    assert!(!started_at.is_empty());
    if amr_wb {
        let prehost = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = state(fixture, "active").await;
                let received = snapshot["call"]["rtp_packets_received"].as_u64().unwrap();
                assert!(received <= 4, "no post-ACK carrier packets have been sent");
                assert_eq!(snapshot["call"]["rtp_packets_sent"], 0);
                assert_eq!(snapshot["call"]["audio_frames_to_host"], 0);
                assert_eq!(snapshot["call"]["rtp_decode_errors"], 0);
                if received == 4 {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("all pre-ACK RTP must enter the actual decoder before host attachment");
        for key in [
            "call_id",
            "codec",
            "audio_port",
            "audio_token",
            "started_at",
        ] {
            assert_eq!(prehost["call"][key], active["call"][key]);
        }
        // The decoder processes packets serially: observing the fourth receive
        // proves the first three finished with peer=None. Code that discarded
        // their PCM cannot pass the subsequent exact four-frame receive.
        client.commands.send(Command::ProbeDialog).await.unwrap();
        assert_eq!(client.next().await, Event::DialogProbed(200));
        let mut unauthorized = b"incorrect-token\0".to_vec();
        unauthorized.extend_from_slice(&pcm[prefix.len()..]);
        host.send_to(&unauthorized, bridge).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(80), host.recv_from(&mut decoded))
                .await
                .is_err(),
            "an incorrect host token must not flush queued PCM"
        );
        let unchanged = state(fixture, "active").await;
        assert_eq!(unchanged["call"]["rtp_packets_received"], 4);
        assert_eq!(unchanged["call"]["audio_frames_to_host"], 0);
        assert_eq!(unchanged["call"]["rtp_packets_sent"], 0);
        // Prefix-only authentication attaches without submitting microphone
        // samples or creating another RTP packet to replace any lost PCM.
        host.send_to(&prefix, bridge).await.unwrap();
    }
    let mut queued_speech = false;
    for _ in 0..4 {
        queued_speech |= offerless_host_pcm(&host, bridge, &prefix).await;
    }
    assert!(
        queued_speech,
        "ACK must release actual pre-answer codec packets to the same host socket"
    );
    let released = media_statistics(fixture, &active, [0, 4, 0, 4]).await;
    assert_eq!(released["call"]["rtp_packets_received"], 4);
    assert_eq!(released["call"]["audio_frames_to_host"], 4);
    assert_eq!(released["call"]["rtp_packets_sent"], 0);
    // The first subsequent carrier packet proves that ACK did not replace the
    // prepared receiver's RTP chronology or lose the original endpoint binding.
    client
        .commands
        .send(Command::SendIncomingAudio)
        .await
        .unwrap();
    let Event::CarrierAudio {
        ssrc,
        sequence,
        timestamp,
        ..
    } = client.next().await
    else {
        panic!("post-answer carrier speech")
    };
    let (old_ssrc, old_sequence, old_timestamp) = previous_downlink.unwrap();
    assert_eq!(ssrc, old_ssrc);
    assert_eq!(sequence, old_sequence.wrapping_add(1));
    assert_eq!(timestamp, old_timestamp.wrapping_add(320));
    let _ = offerless_host_pcm(&host, bridge, &prefix).await;
    let mut previous_uplink = None;
    let mut frames = 0;
    for phase in 0..if replay_previous_ack { 4 } else { 3 } {
        if phase > 0 {
            client
                .commands
                .send(Command::ReplayIncomingAck {
                    malicious: phase == 2,
                    previous_dialog: phase == 3,
                })
                .await
                .unwrap();
            assert_eq!(client.next().await, Event::IncomingAckSent);
            client.commands.send(Command::ProbeDialog).await.unwrap();
            assert_eq!(client.next().await, Event::DialogProbed(200));
        }
        let mut peer_speech = false;
        let mut host_speech = false;
        for _ in 0..4 {
            host.send_to(&pcm, bridge).await.unwrap();
            let Event::Audio {
                codec,
                payload,
                ssrc,
                sequence,
                timestamp,
                peer_speech: speech,
                ..
            } = client.next().await
            else {
                panic!("offerless accepted call lost its real codec after ACK phase {phase}")
            };
            assert_eq!(codec, name);
            assert!(payload > 20);
            if let Some((old_ssrc, old_sequence, old_timestamp)) = previous_uplink {
                assert_eq!(ssrc, old_ssrc);
                assert_eq!(sequence, u16::wrapping_add(old_sequence, 1));
                assert_eq!(timestamp, u32::wrapping_add(old_timestamp, 320));
            }
            previous_uplink = Some((ssrc, sequence, timestamp));
            peer_speech |= speech;
            host_speech |= offerless_host_pcm(&host, bridge, &prefix).await;
            frames += 1;
        }
        assert!(peer_speech && host_speech);
        let current = media_statistics(fixture, &active, [frames, 5 + frames, 0, 5 + frames]).await;
        for key in [
            "call_id",
            "codec",
            "audio_port",
            "audio_token",
            "started_at",
        ] {
            assert_eq!(
                current["call"][key], active["call"][key],
                "ACK replay changed {key}"
            );
        }
        assert_eq!(current["call"]["rtp_packets_sent"], frames);
        assert_eq!(current["call"]["rtp_packets_received"], 5 + frames);
        assert_eq!(current["call"]["audio_frames_to_host"], 5 + frames);
        assert_eq!(current["call"]["rtp_decode_errors"], 0);
    }
    assert!(retries >= 2);
    client.commands.send(Command::RemoteHangup).await.unwrap();
    assert_eq!(client.next().await, Event::RemoteByeAccepted);
    retired_media(fixture, &active).await;
    client
        .commands
        .send(Command::ReplayIncomingAck {
            malicious: true,
            previous_dialog: false,
        })
        .await
        .unwrap();
    assert_eq!(client.next().await, Event::IncomingAckSent);
    client.commands.send(Command::ProbeDialog).await.unwrap();
    assert!(matches!(
        client.next().await,
        Event::DialogProbed(200 | 481)
    ));
    retired_media(fixture, &active).await;
}
// Retries remain explicit peer events and are byte-checked by its oracle.
// Only these two permitted TU retransmissions may interleave media/barriers.
async fn reliable_event(client: &mut call_peer::Client, retries: &mut usize) -> Event {
    loop {
        match client.next().await {
            Event::ReliableRetransmitted => *retries += 1,
            Event::AnswerRetransmitted => {}
            event => return event,
        }
    }
}
async fn reliable_incoming_call(
    fixture: &Fixture,
    client: &mut call_peer::Client,
    generation: u64,
    offerless: bool,
    prack_offer_wb: bool,
) {
    let name = if prack_offer_wb { "AMR-WB" } else { "EVS" };
    let (_, before) = http(fixture, "GET", "/api/reader/vowifi/calls/status", json!({})).await;
    client
        .commands
        .send(Command::IncomingReliable {
            offerless,
            prack_offer_wb,
        })
        .await
        .unwrap();
    if offerless {
        assert_eq!(client.next().await, Event::OfferlessTrying);
    }
    let Event::ReliableProvisional { codec, rseq } = client.next().await else {
        panic!("initial reliable180 with actual SDP")
    };
    assert_eq!(codec, "EVS");
    assert!(rseq > 0);
    let incoming = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let incoming = state(fixture, "incoming").await;
            if incoming["call"]["media_ready"] == !offerless {
                break incoming;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("reliable180 media receipt");
    assert_ne!(incoming["call"]["call_id"], before["call"]["call_id"]);
    assert_eq!(incoming["call"]["direction"], "incoming");
    assert_eq!(incoming["call"]["early_media"], !offerless);
    let host = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let bridge = std::net::SocketAddr::from((
        [127, 0, 0, 1],
        incoming["call"]["audio_port"].as_u64().unwrap() as u16,
    ));
    let prefix = format!("{}\0", incoming["call"]["audio_token"].as_str().unwrap()).into_bytes();
    let mut pcm = prefix.clone();
    for sample in 0..320 {
        pcm.extend((((sample % 40) as i16 - 20) * 500).to_le_bytes());
    }
    // Simulate even a misbehaving host submitting speech before the user has
    // accepted. Native RX may run; this must never authorize voice uplink.
    host.send_to(&pcm, bridge).await.unwrap();
    let mut retries = 0;
    let mut previous_downlink = None;
    let mut early_speech = false;
    for _ in 0..4 {
        client
            .commands
            .send(Command::SendIncomingAudio)
            .await
            .unwrap();
        let Event::CarrierAudio {
            payload,
            ssrc,
            sequence,
            timestamp,
        } = reliable_event(client, &mut retries).await
        else {
            panic!("pre-PRACK encoded EVS")
        };
        assert!(payload > 20);
        if let Some((old_ssrc, old_sequence, old_timestamp)) = previous_downlink {
            assert_eq!(ssrc, old_ssrc);
            assert_eq!(sequence, u16::wrapping_add(old_sequence, 1));
            assert_eq!(timestamp, u32::wrapping_add(old_timestamp, 320));
        }
        previous_downlink = Some((ssrc, sequence, timestamp));
        if !offerless {
            early_speech |= offerless_host_pcm(&host, bridge, &prefix).await;
        }
    }
    let mut decoded = [0u8; 2048];
    if offerless {
        assert!(
            tokio::time::timeout(Duration::from_millis(80), host.recv_from(&mut decoded))
                .await
                .is_err(),
            "unanswered reliable localoffer cannot decode"
        );
    } else {
        assert!(early_speech, "real early EVS RX before user answer");
    }
    let (status, queued_answer) = http(
        fixture,
        "POST",
        "/api/reader/vowifi/calls/answer",
        json!({"generation":generation}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{queued_answer}");
    let waiting = if offerless {
        // No host frame has been received: preserve the immediate zero checks.
        state(fixture, "incoming").await
    } else {
        media_statistics(fixture, &incoming, [0, 4, 0, 4]).await
    };
    assert_eq!(waiting["call"]["call_id"], incoming["call"]["call_id"]);
    assert_eq!(waiting["call"]["rtp_packets_sent"], 0);
    assert_eq!(
        waiting["call"]["rtp_packets_received"],
        if offerless { 0 } else { 4 }
    );
    assert_eq!(
        waiting["call"]["audio_frames_to_host"],
        if offerless { 0 } else { 4 }
    );
    let (dtmf_status, _) = http(
        fixture,
        "POST",
        "/api/reader/vowifi/calls/dtmf",
        json!({"generation":generation,"digit":"#"}),
    )
    .await;
    assert_eq!(dtmf_status, StatusCode::BAD_GATEWAY);
    for (rack, expected) in [(RackCase::Malformed, 400), (RackCase::WrongRseq, 481)] {
        client
            .commands
            .send(Command::ReliablePrack { rack })
            .await
            .unwrap();
        assert_eq!(
            reliable_event(client, &mut retries).await,
            Event::ReliablePrackCompleted {
                status: expected,
                codec: "EVS"
            }
        );
        let unchanged = state(fixture, "incoming").await;
        for key in [
            "call_id",
            "audio_port",
            "audio_token",
            "codec",
            "media_ready",
            "rtp_packets_sent",
            "rtp_packets_received",
            "audio_frames_to_host",
        ] {
            assert_eq!(
                unchanged["call"][key], waiting["call"][key],
                "invalid RAck changed {key}"
            );
        }
    }
    while retries < 2 {
        assert_eq!(client.next().await, Event::ReliableRetransmitted);
        retries += 1;
    }
    // On this ordered connection the peer rejects any final200 until this
    // exact PRACK receives200. Answer-before-PRACK therefore has a wire oracle.
    client
        .commands
        .send(Command::ReliablePrack {
            rack: RackCase::Valid,
        })
        .await
        .unwrap();
    assert_eq!(
        reliable_event(client, &mut retries).await,
        Event::ReliablePrackCompleted {
            status: 200,
            codec: name
        }
    );
    assert_eq!(
        reliable_event(client, &mut retries).await,
        Event::ReliableFinalPendingAck
    );
    let connecting = state(fixture, "connecting").await;
    assert_eq!(connecting["call"]["codec"], name);
    assert_eq!(connecting["call"]["media_ready"], true);
    assert_eq!(connecting["call"]["early_media"], true);
    for key in ["call_id", "audio_port", "audio_token"] {
        assert_eq!(connecting["call"][key], incoming["call"][key]);
    }
    if offerless {
        for _ in 0..4 {
            early_speech |= offerless_host_pcm(&host, bridge, &prefix).await;
        }
        assert!(
            early_speech,
            "PRACKanswer releases pre-answer real codec queue"
        );
    }
    // The final200 carries no SDP and still grants RX only. Across an EVS→WB
    // PRACK offer, the same endpoint and carrier chronology must remain usable.
    host.send_to(&pcm, bridge).await.unwrap();
    let mut selected_speech = false;
    for _ in 0..4 {
        client
            .commands
            .send(Command::SendIncomingAudio)
            .await
            .unwrap();
        let Event::CarrierAudio {
            payload,
            ssrc,
            sequence,
            timestamp,
        } = reliable_event(client, &mut retries).await
        else {
            panic!("selected pre-ACK codec speech")
        };
        assert!(payload > 20);
        let (old_ssrc, old_sequence, old_timestamp) = previous_downlink.unwrap();
        assert_eq!(ssrc, old_ssrc);
        assert_eq!(sequence, old_sequence.wrapping_add(1));
        assert_eq!(timestamp, old_timestamp.wrapping_add(320));
        previous_downlink = Some((ssrc, sequence, timestamp));
        selected_speech |= offerless_host_pcm(&host, bridge, &prefix).await;
    }
    assert!(selected_speech);
    let waiting_ack = media_statistics(fixture, &connecting, [0, 8, 0, 8]).await;
    assert_eq!(waiting_ack["call"]["rtp_packets_sent"], 0);
    assert_eq!(waiting_ack["call"]["rtp_packets_received"], 8);
    assert_eq!(waiting_ack["call"]["audio_frames_to_host"], 8);
    let (dtmf_status, _) = http(
        fixture,
        "POST",
        "/api/reader/vowifi/calls/dtmf",
        json!({"generation":generation,"digit":"#"}),
    )
    .await;
    assert_eq!(dtmf_status, StatusCode::BAD_GATEWAY);
    client.commands.send(Command::AckIncoming).await.unwrap();
    assert_eq!(
        reliable_event(client, &mut retries).await,
        Event::IncomingAckSent
    );
    let active = state(fixture, "active").await;
    assert_eq!(active["call"]["early_media"], false);
    assert!(!active["call"]["started_at"].as_str().unwrap().is_empty());
    for key in ["call_id", "audio_port", "audio_token", "codec"] {
        assert_eq!(active["call"][key], connecting["call"][key]);
    }
    let mut previous_uplink = None;
    let mut frames = 0;
    // Four independent real speech barriers surround duplicate PRACK, malicious
    // duplicate ACK and, in the new EVS dialog, the old WB PRACK transaction.
    for phase in 0..if offerless { 4 } else { 3 } {
        if phase == 1 || phase == 3 {
            client
                .commands
                .send(Command::ReplayReliablePrack {
                    previous_dialog: phase == 3,
                    malicious: true,
                })
                .await
                .unwrap();
            assert_eq!(client.next().await, Event::ReliablePrackReplayed);
        } else if phase == 2 {
            client
                .commands
                .send(Command::ReplayIncomingAck {
                    previous_dialog: false,
                    malicious: true,
                })
                .await
                .unwrap();
            assert_eq!(client.next().await, Event::IncomingAckSent);
        }
        if phase > 0 {
            client.commands.send(Command::ProbeDialog).await.unwrap();
            assert_eq!(client.next().await, Event::DialogProbed(200));
        }
        let mut peer_speech = false;
        let mut host_speech = false;
        for _ in 0..4 {
            host.send_to(&pcm, bridge).await.unwrap();
            let Event::Audio {
                codec,
                payload,
                ssrc,
                sequence,
                timestamp,
                peer_speech: speech,
                ..
            } = client.next().await
            else {
                panic!("reliable accepted codec phase {phase}")
            };
            assert_eq!(codec, name);
            assert!(payload > 20);
            if let Some((old_ssrc, old_sequence, old_timestamp)) = previous_uplink {
                assert_eq!(ssrc, old_ssrc);
                assert_eq!(sequence, u16::wrapping_add(old_sequence, 1));
                assert_eq!(timestamp, u32::wrapping_add(old_timestamp, 320));
            }
            previous_uplink = Some((ssrc, sequence, timestamp));
            peer_speech |= speech;
            host_speech |= offerless_host_pcm(&host, bridge, &prefix).await;
            frames += 1;
        }
        assert!(peer_speech && host_speech);
        let current = media_statistics(fixture, &active, [frames, 8 + frames, 0, 8 + frames]).await;
        for key in [
            "call_id",
            "audio_port",
            "audio_token",
            "codec",
            "started_at",
        ] {
            assert_eq!(
                current["call"][key], active["call"][key],
                "replay changed {key}"
            );
        }
        assert_eq!(current["call"]["rtp_packets_sent"], frames);
        assert_eq!(current["call"]["rtp_packets_received"], 8 + frames);
        assert_eq!(current["call"]["audio_frames_to_host"], 8 + frames);
        assert_eq!(current["call"]["rtp_decode_errors"], 0);
    }
    let (dtmf_status, dtmf) = http(
        fixture,
        "POST",
        "/api/reader/vowifi/calls/dtmf",
        json!({"generation":generation,"digit":"#"}),
    )
    .await;
    assert_eq!(dtmf_status, StatusCode::OK, "{dtmf}");
    assert_eq!(client.next().await, Event::Dtmf);
    client.commands.send(Command::RemoteHangup).await.unwrap();
    assert_eq!(client.next().await, Event::RemoteByeAccepted);
    retired_media(fixture, &active).await;
    client
        .commands
        .send(Command::ReplayReliablePrack {
            previous_dialog: false,
            malicious: true,
        })
        .await
        .unwrap();
    assert_eq!(client.next().await, Event::ReliablePrackReplayed);
    client.commands.send(Command::ProbeDialog).await.unwrap();
    assert!(matches!(
        client.next().await,
        Event::DialogProbed(200 | 481)
    ));
    retired_media(fixture, &active).await;
}
async fn scenario() {
    // A missing platform codec is a failed proof, never a passing media skip.
    let (evs, modes) = proletarius_call::evs::available_audio_codecs(false);
    assert!(
        evs && modes.contains(&8),
        "actual EVS and AMR-WB mode8 codec build is required for this production composition proof"
    );
    let fixture = Fixture::new();
    install_synthetic_idle_module_call_observer(&fixture).await;
    let (peer, mut client) = ike_peer::Peer::with_calls().await;
    let (status, value) = http(
        &fixture,
        "POST",
        "/api/reader/vowifi",
        json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true,"proxy":peer.proxy.to_string(),"epdg":"epdg.invalid","epdg_port":500}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{value}");
    let binding = fixture.registered().await;
    assert_eq!(binding.generation.value(), 1);
    let idle = state(&fixture, "idle").await;
    let generation = idle["generation"].as_u64().unwrap();
    assert!(generation > 0);
    let (status, dial) = http(
        &fixture,
        "POST",
        "/api/reader/vowifi/calls/dial",
        json!({"generation":generation,"peer":call_peer::PEER}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{dial}");
    assert_eq!(client.next().await, Event::Invite);
    let ringing = state(&fixture, "early").await;
    assert_eq!(ringing["call"]["early_media"], false);
    assert_eq!(ringing["call"]["media_ready"], false);
    assert!(ringing["call"]["audio_port"].as_u64().unwrap() > 0);
    assert!(!ringing["call"]["audio_token"].as_str().unwrap().is_empty());
    client
        .commands
        .send(Command::ReliableRinging)
        .await
        .unwrap();
    assert_eq!(client.next().await, Event::Prack);
    let reliable = state(&fixture, "early").await;
    assert_eq!(reliable["call"]["early_media"], false);
    assert_eq!(reliable["call"]["media_ready"], false);
    client.commands.send(Command::AnswerOutgoing).await.unwrap();
    assert_eq!(client.next().await, Event::Ack);
    let active = state(&fixture, "active").await;
    assert_eq!(active["call"]["direction"], "outgoing");
    media(&fixture, &mut client, &active, "EVS").await;
    renegotiate_on_the_live_dialog(&fixture, &mut client, &active, false).await;
    completed_on_the_live_dialog(&fixture, &mut client, &active, "EVS").await;
    let (status, dtmf) = http(
        &fixture,
        "POST",
        "/api/reader/vowifi/calls/dtmf",
        json!({"generation":generation,"digit":"#"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{dtmf}");
    assert_eq!(client.next().await, Event::Dtmf);
    let (status, hangup) = http(
        &fixture,
        "POST",
        "/api/reader/vowifi/calls/hangup",
        json!({"generation":generation}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hangup}");
    assert_eq!(client.next().await, Event::Bye);
    retired_media(&fixture, &active).await;
    replay_accepted_transactions(&mut client, 7, false, false).await;
    replay_completed_transactions(&mut client, 5, false, false).await;
    retired_media(&fixture, &active).await;
    client
        .commands
        .send(Command::IncomingDeferredAck)
        .await
        .unwrap();
    assert_eq!(client.next().await, Event::Ringing);
    let incoming = state(&fixture, "incoming").await;
    assert_eq!(incoming["call"]["direction"], "incoming");
    assert_eq!(incoming["call"]["peer"], call_peer::PEER);
    let (status, answer) = http(
        &fixture,
        "POST",
        "/api/reader/vowifi/calls/answer",
        json!({"generation":generation}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(client.next().await, Event::AnswerPendingAck);
    let active = incoming_media_before_initial_ack(&fixture, &mut client).await;
    media(&fixture, &mut client, &active, "AMR-WB").await;
    amr_wb_cmr_on_the_live_dialog(&fixture, &mut client, &active).await;
    renegotiate_on_the_live_dialog(&fixture, &mut client, &active, true).await;
    completed_on_the_live_dialog(&fixture, &mut client, &active, "AMR-WB").await;
    client.commands.send(Command::RemoteHangup).await.unwrap();
    assert_eq!(client.next().await, Event::RemoteByeAccepted);
    retired_media(&fixture, &active).await;
    replay_accepted_transactions(&mut client, 7, false, false).await;
    replay_completed_transactions(&mut client, 5, false, false).await;
    retired_media(&fixture, &active).await;
    // A late original ACK after BYE must not attach or restart retired media.
    // OPTIONS and owner INFO follow on the same protected TCP stream; INFO is
    // an explicit processing barrier rather than a timing assumption.
    client.commands.send(Command::AckIncoming).await.unwrap();
    assert_eq!(client.next().await, Event::IncomingAckSent);
    client.commands.send(Command::ProbeDialog).await.unwrap();
    assert!(matches!(
        client.next().await,
        Event::DialogProbed(200 | 481)
    ));
    retired_media(&fixture, &active).await;
    offerless_incoming_call(&fixture, &mut client, generation, false, false).await;
    offerless_incoming_call(&fixture, &mut client, generation, true, true).await;
    reliable_incoming_call(&fixture, &mut client, generation, false, false).await;
    reliable_incoming_call(&fixture, &mut client, generation, false, true).await;
    reliable_incoming_call(&fixture, &mut client, generation, true, false).await;
    // Same registered generation remained the sole signaling and lease owner.
    assert_eq!(
        fixture.service.sms_binding().unwrap().binding,
        binding.binding
    );
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    gateway::stop(&fixture).await;
    {
        let record = fixture.service.record().unwrap().unwrap();
        let current = record.driver.current.lock().unwrap();
        let result = current
            .result
            .as_ref()
            .expect("joined actual production worker");
        let ordinary::Outcome::Running(Ok(exit)) = &result.outcome else {
            panic!("actual registered worker failed")
        };
        let consumer = exit.consumer.as_ref().unwrap();
        assert!(consumer.media_cleanup.is_ok());
        assert!(consumer.timer_failures.is_empty());
        assert!(result.lease_release.as_ref().is_some_and(Result::is_ok));
    }
    let rows = proletarius_store::ArchiveStore::new(fixture.root.join("calls.json"))
        .load()
        .unwrap();
    assert_eq!(
        rows.len(),
        7,
        "the original four dialogs and three reliable dialogs must each be durable: {rows:?}"
    );
    let ids = rows
        .iter()
        .map(|row| {
            row["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .expect("durable call id")
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        ids.len(),
        7,
        "every wire dialog needs its own durable id: {rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row["direction"] == "out").count(),
        1,
        "{rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row["direction"] == "in").count(),
        6,
        "{rows:?}"
    );
    // The archive intentionally has no codec or SIP Call-ID fields. These
    // seven calls ran serially in this exact order; match each durable interval
    // to its scenario rather than inventing archive fields or accepting extras.
    let mut chronological = rows
        .iter()
        .map(|row| {
            let started = time::OffsetDateTime::parse(
                row["started_at"].as_str().unwrap(),
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap();
            let ended = time::OffsetDateTime::parse(
                row["ended_at"].as_str().unwrap(),
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap();
            (started, ended, row)
        })
        .collect::<Vec<_>>();
    chronological.sort_by_key(|(started, _, _)| *started);
    for ((started, ended, row), (scenario, direction)) in chronological.iter().zip([
        ("original outgoing EVS", "out"),
        ("original incoming AMR-WB", "in"),
        ("offerless incoming EVS", "in"),
        ("offerless incoming AMR-WB", "in"),
        ("reliable incoming EVS with empty PRACK", "in"),
        ("reliable incoming EVS with PRACK AMR-WB offer", "in"),
        ("reliable offerless incoming with PRACK EVS answer", "in"),
    ]) {
        assert_eq!(
            row["direction"], direction,
            "missing durable {scenario}: {rows:?}"
        );
        assert!(
            *ended >= *started,
            "invalid durable interval for {scenario}"
        );
    }
    for adjacent in chronological.windows(2) {
        assert!(
            adjacent[0].1 <= adjacent[1].0,
            "serial dialogs must retain distinct durable intervals: {rows:?}"
        );
    }
    assert!(
        rows.iter().all(|v| v["number"] == call_peer::PEER),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .all(|v| v["answered"] == true && v["device"] == "reader"),
        "{rows:?}"
    );
    let trace = peer.trace.clone();
    fixture.finish().await;
    peer.finish().await;
    assert_eq!(trace.ike.load(Ordering::SeqCst), 1);
    assert_eq!(trace.registers.load(Ordering::SeqCst), 1);
    assert_eq!(trace.deletes.load(Ordering::SeqCst), 1);
}
