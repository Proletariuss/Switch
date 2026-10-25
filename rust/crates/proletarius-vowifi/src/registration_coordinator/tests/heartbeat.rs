//! Real userspace IP/TCP/ESP -> native profile REGISTER -> OPTIONS exchanges.
//! No host network, DNS, SIM or keychain is used.
use super::*;
use crate::{
    ims_network_runtime::ImsNetworkRuntime,
    sip_session_runtime::{
        HeartbeatFailure, RegisteredSipEvent, RegisteredSipMessage, RegisteredSipRuntime,
    },
};
use proletarius_sip::{
    Header, Headers, Message, MessageData, Method, Parser, Request, Response, Uri,
};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    CancelledWrite,
    HealthyConcurrentSms,
    Recovered,
    MismatchAndProvisional,
    Blackhole,
    StopBeforeProbe,
    MissingConfiguration,
    LegacyConstructor,
}
fn response(request: &Request, status: u16) -> Vec<u8> {
    let mut headers = Headers::default();
    for name in ["Via", "From", "To", "Call-ID", "CSeq"] {
        headers.push(Header::new(name, request.data.headers.get(name).unwrap().value()).unwrap());
    }
    Message::Response(Response {
        status,
        reason: "fixture".into(),
        data: MessageData::new("SIP/2.0", headers, vec![]),
    })
    .encode()
}
async fn next_request(reader: &mut SipStreamReader<ReadHalf<ManagedTcpStream>>) -> Request {
    let Some(SipStreamItem::Message(wire)) = reader.next().await.unwrap() else {
        panic!("actual peer SIP request missing")
    };
    let Message::Request(request) = Parser::default().parse(&wire, true).unwrap().0 else {
        panic!("expected SIP request")
    };
    request
}
fn sms_request() -> Request {
    let mut headers = Headers::default();
    for (name, value) in [
        (
            "Via",
            "SIP/2.0/TCP 10.0.0.2:5064;branch=z9hG4bK-concurrent-sms",
        ),
        ("From", "<sip:+447700900123@ims.example>;tag=sms"),
        ("To", "<sip:smsc@ims.example>"),
        ("Call-ID", "concurrent-sms"),
        ("CSeq", "77 MESSAGE"),
        ("Content-Type", "application/vnd.3gpp.sms"),
    ] {
        headers.push(Header::new(name, value).unwrap());
    }
    Request {
        method: Method::MESSAGE,
        uri: Uri::parse("sip:smsc@ims.example").unwrap(),
        data: MessageData::new("SIP/2.0", headers, vec![0, 1, 0]),
    }
}
async fn exercise(mode: Mode) {
    let config = |address: &str| TunnelConfiguration {
        ipv4_addresses: vec![address.parse().unwrap()],
        ..Default::default()
    };
    let (lw, lr) = watch::channel(0);
    let (rw, rr) = watch::channel(0);
    let (tcp, security, _udp, network) = ImsNetworkRuntime::new_with_udp(lw);
    let (peer, peer_security, _peer_udp, peer_network) = ImsNetworkRuntime::new_with_udp(rw);
    let retired = network.retirement();
    let peer_retired = peer_network.retirement();
    let (ab, br) = mpsc::channel(256);
    let (ba, ar) = mpsc::channel(256);
    let (raw, mut packets) = mpsc::channel(256);
    let (hold, mut held) = watch::channel(false);
    let (stop, stopped) = watch::channel(false);
    let mut relay_stop = stopped.clone();
    let relay = tokio::spawn(async move {
        loop {
            let packet = tokio::select! {_=relay_stop.changed()=>break,packet=packets.recv()=>match packet{Some(packet)=>packet,None=>break}};
            while *held.borrow() {
                tokio::select! {_=relay_stop.changed()=>return,_=held.changed()=>{}}
            }
            if ba.send(packet).await.is_err() {
                break;
            }
        }
    });
    let client_network = tokio::spawn(
        TunnelNetwork::new(&config("10.0.0.2"))
            .unwrap()
            .run_managed(SocketSet::new(vec![]), ar, ab, stopped.clone(), lr, network),
    );
    let server_network = tokio::spawn(
        TunnelNetwork::new(&config("10.20.30.9"))
            .unwrap()
            .run_managed(SocketSet::new(vec![]), br, raw, stopped, rr, peer_network),
    );
    let (ready, armed) = tokio::sync::oneshot::channel();
    let server = async {
        let lease = peer_security
            .install(full_gate_server_policy())
            .await
            .unwrap();
        let protected = peer
            .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
            .await
            .unwrap();
        let plain = peer
            .listen(5060, Some("10.0.0.2:5050".parse().unwrap()))
            .await
            .unwrap();
        ready.send(()).unwrap();
        let stream = plain.accept().await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = SipStreamReader::new(read);
        let initial = next_request(&mut reader).await;
        assert_eq!(initial.method, Method::REGISTER);
        let challenge = format!(
            "SIP/2.0 401 Challenge\r\nCall-ID: heartbeat-register\r\nCSeq: 1 REGISTER\r\nWWW-Authenticate: Digest realm=\"ims.example\",nonce=\"{}\",algorithm=AKAv1-MD5\r\nSecurity-Server: ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc;prot=esp;mod=trans;spi-c=111;spi-s=222;port-c=6050;port-s=6060\r\nContent-Length: 0\r\n\r\n",
            "01".repeat(32)
        );
        write.write_all(challenge.as_bytes()).await.unwrap();
        write.flush().await.unwrap();
        assert!(reader.next().await.unwrap().is_none());
        reader.into_inner().unsplit(write).close().await.unwrap();
        let stream = protected.accept().await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = SipStreamReader::new(read);
        let register = next_request(&mut reader).await;
        assert_eq!(register.method, Method::REGISTER);
        assert!(register.data.headers.get("Security-Verify").is_some());
        write.write_all(&response(&register, 200)).await.unwrap();
        write.flush().await.unwrap();
        if mode == Mode::CancelledWrite {
            hold.send_replace(true);
        }
        if !matches!(
            mode,
            Mode::StopBeforeProbe | Mode::MissingConfiguration | Mode::LegacyConstructor
        ) {
            let first = next_request(&mut reader).await;
            assert_eq!(first.method, Method::OPTIONS);
            assert_eq!(first.uri.to_string(), "sip:10.20.30.9:5060");
            assert_eq!(
                first.data.headers.get("Route").unwrap().value(),
                "<sip:10.20.30.9:5060;lr>"
            );
            assert_eq!(
                first
                    .data
                    .headers
                    .get("P-Preferred-Identity")
                    .unwrap()
                    .value(),
                "<sip:+447700900123@ims.example>"
            );
            assert!(
                first
                    .data
                    .headers
                    .get("Via")
                    .unwrap()
                    .value()
                    .starts_with("SIP/2.0/TCP 10.0.0.2:5064;branch=z9hG4bK")
            );
            assert!(
                first
                    .data
                    .headers
                    .get("Security-Verify")
                    .unwrap()
                    .value()
                    .contains("ipsec-3gpp")
            );
            if matches!(mode, Mode::HealthyConcurrentSms | Mode::CancelledWrite) {
                if mode == Mode::HealthyConcurrentSms {
                    let sms = next_request(&mut reader).await;
                    assert_eq!(sms.method, Method::MESSAGE);
                    write.write_all(&response(&sms, 200)).await.unwrap();
                }
                write.write_all(&response(&first, 200)).await.unwrap();
                write.write_all(b"\r\n").await.unwrap();
                write.flush().await.unwrap();
            } else {
                if mode == Mode::Recovered {
                    write.write_all(&response(&first, 503)).await.unwrap();
                }
                if mode == Mode::MismatchAndProvisional {
                    let mut wrong = response(&first, 200);
                    let text = String::from_utf8(wrong)
                        .unwrap()
                        .replace("z9hG4bK", "z9hG4bK-wrong-");
                    wrong = text.into_bytes();
                    write.write_all(&wrong).await.unwrap();
                    write.write_all(&response(&first, 100)).await.unwrap();
                }
                write.flush().await.unwrap();
                let confirmation = next_request(&mut reader).await;
                assert_eq!(confirmation.method, Method::OPTIONS);
                assert_eq!(
                    confirmation.data.headers.get("CSeq").unwrap().value(),
                    first.data.headers.get("CSeq").unwrap().value()
                );
                assert_ne!(
                    confirmation.data.headers.get("Call-ID").unwrap().value(),
                    first.data.headers.get("Call-ID").unwrap().value()
                );
                assert_ne!(
                    confirmation.data.headers.get("Via").unwrap().value(),
                    first.data.headers.get("Via").unwrap().value()
                );
                if mode != Mode::Blackhole {
                    write
                        .write_all(&response(&confirmation, 200))
                        .await
                        .unwrap();
                    write.flush().await.unwrap();
                }
            }
        }
        assert!(
            reader.next().await.unwrap().is_none(),
            "stop did not close the captured primary socket"
        );
        reader.into_inner().unsplit(write).close().await.unwrap();
        lease.clear().await.unwrap();
    };
    let client = async {
        armed.await.unwrap();
        let sim = crate::carrier::SimIdentity {
            mcc: "234".into(),
            mnc: "33".into(),
            imsi: "234330123456789".into(),
            ..Default::default()
        };
        let carrier = crate::carrier::Registry::embedded()
            .unwrap()
            .resolve(&sim)
            .unwrap()
            .unwrap();
        let overrides=crate::carrier_overrides::OverrideSet::parse(b"carrier_overrides:\n  '234/33': {ims_domain: 'ims.example', ims_realm: 'ims.example', ims_local_port: 5050, ims_identity_source: isim, ims_tcp_keepalive_seconds: 1, ims_options_ping_interval_seconds: 1}\n").unwrap();
        let profile = carrier
            .native_ims_register_profile(&sim, overrides.get("234", "33"))
            .unwrap();
        let device = profile
            .resolve_device_identity(Some("353936932685355"))
            .unwrap();
        let runtime = NativeImsRuntime {
            imsi: &sim.imsi,
            isim_private_identity: Some("subscriber@ims.example"),
            isim_public_identity: Some("sip:+447700900123@ims.example"),
            local_ip: "10.0.0.2".parse().unwrap(),
            pcscf: "10.20.30.9:5060".parse().unwrap(),
            call_id: "heartbeat-register",
            from_tag: "heartbeat-register",
            via_branch: "z9hG4bK-register",
            cell_id: None,
            mcc: &sim.mcc,
            mnc: &sim.mnc,
        };
        let identity = profile.resolve_subscriber_identity(runtime).unwrap();
        let (_cancel, signal) = watch::channel(false);
        let (events, _rx) = mpsc::channel(8);
        let mut session = register_native_profile_with_identity(
            RegistrationServices::new(&tcp, &security, &events),
            &profile,
            &device,
            &identity,
            runtime,
            NativeSecurityEndpoints {
                spi_c: 333,
                spi_s: 444,
                port_c: 5064,
                port_s: 5063,
            },
            Instant::now() + Duration::from_secs(4),
            signal,
            |_, _| async {
                Ok::<_, Infallible>(SimAnswer::Success {
                    res: Zeroizing::new(vec![1; 8]),
                    ck: Zeroizing::new([2; 16]),
                    ik: Zeroizing::new([3; 16]),
                })
            },
        )
        .await
        .ok()
        .expect("real protected REGISTER");
        assert!(session.is_secured());
        assert!(session.belongs_to_tcp(&tcp));
        let health = session.heartbeat_health();
        if mode == Mode::MissingConfiguration {
            // Fault injection only after a real protected REGISTER succeeded:
            // missing native configuration must not become an allowed exemption.
            assert!(session.take_heartbeat_configuration().is_some());
        }
        let mut registered = if mode == Mode::LegacyConstructor {
            RegisteredSipRuntime::new(session)
        } else {
            RegisteredSipRuntime::new_with_heartbeat(session)
        };
        if matches!(mode, Mode::MissingConfiguration | Mode::LegacyConstructor) {
            if mode == Mode::MissingConfiguration {
                let error = registered
                    .heartbeat_tick()
                    .await
                    .expect_err("missing native heartbeat configuration must fail closed");
                assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
                assert!(error.get_ref().is_some_and(|e| e.is::<HeartbeatFailure>()));
                assert_eq!(
                    error.to_string(),
                    "registered IMS heartbeat configuration unavailable"
                );
                assert!(!health.is_healthy());
            } else {
                // The older explicit no-heartbeat constructor keeps its existing
                // contract; only the native constructor requires frozen policy.
                registered.heartbeat_tick().await.unwrap();
                assert!(health.is_healthy());
            }
            registered.shutdown().await.unwrap();
            assert!(!health.is_healthy());
            return;
        }
        registered.shorten_heartbeat_budgets(Duration::from_millis(70), Duration::from_millis(30));
        if mode == Mode::StopBeforeProbe {
            registered.shutdown().await.unwrap();
            assert!(!health.is_healthy());
            return;
        }
        if mode == Mode::CancelledWrite {
            tokio::time::sleep_until(registered.heartbeat_deadline().unwrap()).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(25), registered.next_event())
                    .await
                    .is_err()
            );
            assert!(
                registered.heartbeat_write_pending(),
                "fixture did not hold actual TCP acknowledgement"
            );
            hold.send_replace(false);
            assert!(matches!(
                registered.next_event().await.unwrap(),
                Some(RegisteredSipEvent::KeepAlive { .. })
            ));
            assert!(health.is_healthy());
        } else if mode == Mode::HealthyConcurrentSms {
            tokio::time::sleep_until(registered.heartbeat_deadline().unwrap()).await;
            registered.heartbeat_tick().await.unwrap();
            let sms_ticket = registered
                .send_tracked_request(sms_request(), Duration::from_secs(1))
                .await
                .unwrap();
            assert!(
                matches!(registered.next_event().await.unwrap(),Some(RegisteredSipEvent::Message{response_ticket:Some(ticket),message,..}) if ticket==sms_ticket && matches!(message.as_ref(),RegisteredSipMessage::Response{status:200,..}))
            );
            assert!(matches!(
                registered.next_event().await.unwrap(),
                Some(RegisteredSipEvent::KeepAlive { .. })
            ));
            assert!(health.is_healthy());
        } else {
            loop {
                match registered.next_event().await.unwrap().unwrap() {
                    RegisteredSipEvent::HeartbeatHealthChanged { healthy: false } => break,
                    RegisteredSipEvent::Message {
                        response_ticket: None,
                        ..
                    } if mode == Mode::MismatchAndProvisional => {}
                    _ => panic!("unexpected event before heartbeat miss"),
                }
            }
            assert!(!health.is_healthy());
            if mode == Mode::Blackhole {
                let error = registered
                    .next_event()
                    .await
                    .err()
                    .expect("confirmation blackhole must fail runtime");
                assert!(error.get_ref().is_some_and(|e| e.is::<HeartbeatFailure>()));
            } else {
                assert!(matches!(
                    registered.next_event().await.unwrap(),
                    Some(RegisteredSipEvent::HeartbeatHealthChanged { healthy: true })
                ));
                assert!(health.is_healthy());
            }
        }
        registered.shutdown().await.unwrap();
        assert!(
            !health.is_healthy(),
            "retired handle never remains a readiness authority"
        );
        let listener = tcp
            .listen(5063, None)
            .await
            .expect("actual listener retirement before result");
        listener.close().await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(7), async {
        tokio::join!(client, server);
    })
    .await
    .expect("protected heartbeat fixture stalled");
    stop.send_replace(true);
    assert!(client_network.await.unwrap().is_ok());
    assert!(server_network.await.unwrap().is_ok());
    assert!(retired.confirmed() && peer_retired.confirmed());
    relay.await.unwrap();
}
#[tokio::test]
async fn protected_heartbeat_matches_pong_without_consuming_concurrent_sms_ticket() {
    exercise(Mode::HealthyConcurrentSms).await;
}
#[tokio::test]
async fn primary_rejection_withdraws_health_then_fresh_confirmation_restores_it() {
    exercise(Mode::Recovered).await;
}
#[tokio::test]
async fn wrong_branch_and_provisional_response_do_not_prove_heartbeat_health() {
    exercise(Mode::MismatchAndProvisional).await;
}
#[tokio::test]
async fn blackhole_confirmation_fails_and_retires_actual_session_and_network_owner() {
    exercise(Mode::Blackhole).await;
}
#[tokio::test]
async fn stopped_registered_owner_sends_no_probe_and_confirms_physical_cleanup() {
    exercise(Mode::StopBeforeProbe).await;
}

#[tokio::test]
async fn cancelled_options_write_waiter_resumes_original_frame_without_duplicate_submission() {
    exercise(Mode::CancelledWrite).await;
}

#[tokio::test]
async fn real_registered_session_missing_heartbeat_configuration_fails_closed() {
    exercise(Mode::MissingConfiguration).await;
}

#[tokio::test]
async fn legacy_runtime_constructor_preserves_explicit_no_heartbeat_contract() {
    exercise(Mode::LegacyConstructor).await;
}
