use super::*;
use proletarius_vowifi::carrier::ResolvedCarrier;

pub(super) fn profile(extra: &str) -> NativeImsRegisterProfile {
    let sim = SimIdentity {
        mcc: "234".into(),
        mnc: "33".into(),
        imsi: "234330000000001".into(),
        ..Default::default()
    };
    let carrier = ResolvedCarrier {
        id: "fixture".into(),
        bundle: "".into(),
        selector: "".into(),
        carrier_name: None,
        configuration: json!({}),
    };
    let yaml = format!(
        "carrier_overrides:\n  '234/33':\n    ims_local_port: 5064\n    ims_register_template:\n      sec_agree_mode: disabled\n{extra}"
    );
    let overrides = OverrideSet::parse(yaml.as_bytes()).unwrap();
    carrier
        .native_ims_register_profile(&sim, overrides.get("234", "33"))
        .unwrap()
}
#[test]
fn dual_stack_prefers_all_ipv6_candidates_in_advertised_order() {
    let tunnel = TunnelConfiguration {
        ipv4_addresses: vec!["10.0.0.2".parse().unwrap()],
        ipv6_addresses: vec![("2001:db8::2".parse().unwrap(), 64)],
        pcscf_servers: vec![
            "10.0.0.9".parse().unwrap(),
            "2001:db8::9".parse().unwrap(),
            "2001:db8::8".parse().unwrap(),
            "2001:db8::9".parse().unwrap(),
        ],
        ..Default::default()
    };
    let candidates = select(&profile(""), &tunnel, RequestedIpStack::Dual).unwrap();
    assert_eq!(
        candidates.iter().map(|v| v.registrar).collect::<Vec<_>>(),
        vec![
            "[2001:db8::9]:5060".parse().unwrap(),
            "[2001:db8::8]:5060".parse().unwrap()
        ]
    );
    assert!(
        candidates
            .iter()
            .all(|v| v.local_ip == "2001:db8::2".parse::<IpAddr>().unwrap())
    );
    assert!(
        select(&profile(""), &tunnel, RequestedIpStack::Ipv4).unwrap()[0]
            .local_ip
            .is_ipv4()
    );
}
#[test]
fn local_registrar_uses_gateway_without_changing_sip_registrar() {
    let tunnel = TunnelConfiguration {
        ipv4_addresses: vec!["10.0.0.2".parse().unwrap()],
        pcscf_servers: vec!["10.0.0.2".parse().unwrap(), "10.0.0.9".parse().unwrap()],
        ..Default::default()
    };
    let candidates = select(&profile(""), &tunnel, RequestedIpStack::Ipv4).unwrap();
    assert_eq!(candidates[0].registrar, "10.0.0.2:5060".parse().unwrap());
    assert_eq!(candidates[0].transport, "10.0.0.9:5060".parse().unwrap());
}
#[test]
fn explicit_override_keeps_port_and_requires_matching_requested_family() {
    let profile = profile("    ims_pcscf: '10.0.0.8:5090'\n");
    let tunnel = TunnelConfiguration {
        ipv4_addresses: vec!["10.0.0.2".parse().unwrap()],
        ..Default::default()
    };
    assert_eq!(
        select(&profile, &tunnel, RequestedIpStack::Ipv4).unwrap()[0].registrar,
        "10.0.0.8:5090".parse().unwrap()
    );
    assert_eq!(
        select(&profile, &tunnel, RequestedIpStack::Ipv6),
        Err(NativeImsEndpointSelectionError::NoMatchingTunnelAddress)
    );
}
#[test]
fn only_initial_rejections_and_confirmed_transport_failures_can_advance() {
    let p = profile("");
    let initial: RegistrationError<()> = RegistrationError::Sip {
        stage: RegistrationStage::InitialRegister,
        source: SipError::RegistrationRejected(403),
    };
    assert_eq!(
        retry_delay(&p, retry_input(&initial), true, true),
        Some(Duration::from_millis(150))
    );
    assert_eq!(retry_delay(&p, retry_input(&initial), false, true), None);
    assert_eq!(retry_delay(&p, retry_input(&initial), true, false), None);
    let auth: RegistrationError<()> = RegistrationError::Sip {
        stage: RegistrationStage::PlainAuthenticatedRegister,
        source: SipError::RegistrationRejected(403),
    };
    assert_eq!(retry_input(&auth), None);
    let unauthorized: RegistrationError<()> = RegistrationError::Sip {
        stage: RegistrationStage::InitialRegister,
        source: SipError::RegistrationRejected(401),
    };
    assert_eq!(
        retry_delay(&p, retry_input(&unauthorized), true, true),
        None
    );
    let disabled = profile("      register_policy:\n        forbidden_status_codes: []\n");
    assert_eq!(
        retry_delay(&disabled, retry_input(&initial), true, true),
        None
    );
}

#[tokio::test]
async fn actual_register_retries_clean_failure_with_new_ids_and_keeps_winning_gateway() {
    exchange(503, false, false).await;
}
#[tokio::test]
async fn actual_initial_forbidden_uses_the_existing_default_candidate_policy() {
    exchange(403, false, false).await;
}
#[tokio::test]
async fn actual_connection_closed_retries_only_after_exact_socket_retirement() {
    exchange(0, false, false).await;
}
#[tokio::test]
async fn cancelled_retry_gap_does_not_open_the_next_candidate() {
    exchange(503, true, false).await;
}
#[tokio::test]
async fn registered_owner_survives_startup_sender_drop_and_explicit_stop_still_cancels() {
    tokio::time::timeout(Duration::from_secs(5), exchange(503, false, true))
        .await
        .expect("registered owner stop cleanup deadline");
}
async fn exchange(first_status: u16, cancel_gap: bool, stop_after_success: bool) {
    use proletarius_ims::stream::{SipStreamItem, SipStreamReader};
    use proletarius_vowifi::{
        netstack_runtime::TunnelNetwork, netstack_tcp::TcpSocketRegistry,
        security_runtime::SecurityNetworkRuntime,
    };
    use tokio::{
        io::AsyncWriteExt,
        sync::{mpsc, oneshot, watch},
    };
    let config = |ip: &str| TunnelConfiguration {
        ipv4_addresses: vec![ip.parse().unwrap()],
        ..Default::default()
    };
    let local_network = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
    let remote_network = TunnelNetwork::new(&config("10.20.30.9")).unwrap();
    let (local_wake_tx, local_wake) = watch::channel(0u64);
    let (remote_wake_tx, remote_wake) = watch::channel(0u64);
    let (local_tcp, local_registry) = TcpSocketRegistry::new(local_wake_tx.clone());
    let (remote_tcp, remote_registry) = TcpSocketRegistry::new(remote_wake_tx);
    let (security, _security_runtime) = SecurityNetworkRuntime::new(
        |_: &mut smoltcp::iface::Interface, _: &mut smoltcp::iface::SocketSet<'static>| {
            Ok::<(), Infallible>(())
        },
        local_wake_tx,
    );
    let (local_out, remote_in) = mpsc::channel(64);
    let (remote_out, local_in) = mpsc::channel(64);
    let (network_stop, network_stopped) = watch::channel(false);
    let local = tokio::spawn(local_network.run_managed(
        smoltcp::iface::SocketSet::new(vec![]),
        local_in,
        local_out,
        network_stopped.clone(),
        local_wake,
        local_registry,
    ));
    let remote = tokio::spawn(remote_network.run_managed(
        smoltcp::iface::SocketSet::new(vec![]),
        remote_in,
        remote_out,
        network_stopped,
        remote_wake,
        remote_registry,
    ));
    let (session_stop, session_stopped) = watch::channel(false);
    let observation = session_stop.subscribe();
    let startup_owner = session_stop.clone();
    let explicit_stop = stop_after_success.then(|| session_stop.clone());
    let gap_stop = cancel_gap.then(|| session_stop.clone());
    drop(session_stop);
    let (ready, ready_rx) = oneshot::channel();
    let (closed, closed_rx) = oneshot::channel();
    let (local_closed, local_closed_rx) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let first = remote_tcp
            .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
            .await
            .unwrap();
        let mut second = remote_tcp
            .listen(6061, Some("10.0.0.2:5064".parse().unwrap()))
            .await
            .unwrap();
        ready.send(()).unwrap();
        let stream = first.accept().await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = SipStreamReader::new(read);
        let Some(SipStreamItem::Message(bytes)) = reader.next().await.unwrap() else {
            panic!("first REGISTER")
        };
        let initial = String::from_utf8(bytes.to_vec()).unwrap();
        let header = |text: &str, name: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(name).map(str::trim))
                .unwrap()
                .to_string()
        };
        let first_id = header(&initial, "Call-ID:");
        assert!(initial.starts_with("REGISTER "));
        if first_status != 0 {
            write.write_all(format!("SIP/2.0 {first_status} Fixture\r\nCall-ID: {first_id}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n",header(&initial,"CSeq:")).as_bytes()).await.unwrap();
            if cancel_gap {
                gap_stop.as_ref().unwrap().send(true).unwrap();
            }
            // The next attempt uses the same local port only after the first
            // coordinator awaited actual removal. The peer sees the exact EOF.
            assert!(reader.next().await.unwrap().is_none());
            reader.into_inner().unsplit(write).close().await.unwrap();
        } else {
            reader.into_inner().unsplit(write).close().await.unwrap();
        }
        if cancel_gap {
            assert!(
                tokio::time::timeout(Duration::from_millis(200), second.accept_cancel_safe())
                    .await
                    .is_err()
            );
            second.close().await.unwrap();
            closed.send(()).unwrap();
        } else {
            let stream = second.accept().await.unwrap();
            let (read, mut write) = tokio::io::split(stream);
            let mut reader = SipStreamReader::new(read);
            let Some(SipStreamItem::Message(bytes)) = reader.next().await.unwrap() else {
                panic!("second REGISTER")
            };
            let retry = String::from_utf8(bytes.to_vec()).unwrap();
            assert_ne!(header(&retry, "Call-ID:"), first_id);
            assert_ne!(header(&retry, "From:"), header(&initial, "From:"));
            assert_ne!(header(&retry, "Via:"), header(&initial, "Via:"));
            write.write_all(format!("SIP/2.0 200 OK\r\nCall-ID: {}\r\nCSeq: {}\r\nExpires: 3600\r\nP-Associated-URI: <sip:reader@ims.example>\r\nContent-Length: 0\r\n\r\n",header(&retry,"Call-ID:"),header(&retry,"CSeq:")).as_bytes()).await.unwrap();
            if stop_after_success {
                // Explicit cancellation retires the client with Abort. Local
                // removal proves ownership cleanup, not remote EOF delivery.
                // Wait for that real cleanup, then retire this exact peer too.
                local_closed_rx.await.unwrap();
            } else {
                assert!(reader.next().await.unwrap().is_none());
            }
            reader.into_inner().unsplit(write).close().await.unwrap();
            closed.send(()).unwrap();
        }
    });
    ready_rx.await.unwrap();
    let profile = profile("");
    let device = profile
        .resolve_device_identity(Some("356938035643809"))
        .unwrap();
    let runtime = NativeImsRuntime {
        imsi: "234330000000001",
        mcc: "234",
        mnc: "33",
        local_ip: "10.0.0.2".parse().unwrap(),
        pcscf: "10.20.30.9:6060".parse().unwrap(),
        call_id: "candidate-first",
        from_tag: "first-tag",
        via_branch: "z9hG4bK-first",
        isim_private_identity: None,
        isim_public_identity: None,
        cell_id: None,
    };
    let subscriber = profile.resolve_subscriber_identity(runtime).unwrap();
    let candidates = [
        Candidate {
            local_ip: runtime.local_ip,
            registrar: runtime.pcscf,
            transport: runtime.pcscf,
        },
        Candidate {
            local_ip: runtime.local_ip,
            registrar: "10.0.0.2:6061".parse().unwrap(),
            transport: "10.20.30.9:6061".parse().unwrap(),
        },
    ];
    let (events, _received) = mpsc::channel(16);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        super::super::finish_owned_registration(
            register(
                RegistrationServices::new(&local_tcp, &security, &events),
                &profile,
                &device,
                &subscriber,
                runtime,
                NativeSecurityEndpoints {
                    spi_c: 1,
                    spi_s: 2,
                    port_c: 5064,
                    port_s: 5063,
                },
                None,
                &candidates,
                tokio::time::Instant::now() + Duration::from_secs(4),
                session_stopped,
                |_, _| async {
                    panic!("initial fixtures must not perform AKA");
                    #[allow(unreachable_code)]
                    Err(NativeAkaError::Lease(
                        proletarius_domain::BackendFailure::Io("unexpected AKA".into()),
                    ))
                },
            ),
            std::future::pending(),
            startup_owner,
        ),
    )
    .await
    .expect("candidate sequence deadline");
    if cancel_gap {
        assert!(matches!(
            result,
            Err(NativeImsRegistrationError::Registration(_))
        ));
    } else {
        let mut session = result.unwrap_or_else(|_| panic!("candidate should succeed"));
        // No caller or server sender survives the startup handoff here.
        // Previously this was closed deterministically after helper success.
        assert!(
            observation.has_changed().is_ok(),
            "startup dropped the registered cancellation owner"
        );
        let (foreign, _) = watch::channel(false);
        assert_eq!(
            session.retain_cancellation_owner(foreign),
            Err("registered_cancellation_owner_mismatch")
        );
        assert!(observation.has_changed().is_ok());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), session.next_event())
                .await
                .is_err(),
            "registered channel was already closed"
        );
        if let Some(stop) = explicit_stop.as_ref() {
            stop.send(true).unwrap();
            let error = session
                .next_event()
                .await
                .err()
                .expect("explicit stop must cancel session");
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        }
        assert_eq!(session.signaling_peer(), candidates[1].transport);
        assert_eq!(
            session.call_context().unwrap().peer(),
            candidates[1].registrar
        );
        session.shutdown().await.unwrap();
    }
    let _ = local_closed.send(());
    closed_rx.await.unwrap();
    peer.await.unwrap();
    drop(explicit_stop);
    assert!(
        observation.has_changed().is_err(),
        "shutdown retained its cancellation sender"
    );
    network_stop.send(true).unwrap();
    local.await.unwrap().unwrap();
    remote.await.unwrap().unwrap();
}
