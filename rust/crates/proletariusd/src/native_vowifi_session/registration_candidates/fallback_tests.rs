use super::*;
use proletarius_ims::stream::{SipStreamItem, SipStreamReader};
use proletarius_vowifi::{
    netstack_runtime::TunnelNetwork, netstack_tcp::TcpSocketRegistry,
    security_runtime::SecurityNetworkRuntime,
};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, watch};
#[derive(Clone, Copy)]
enum Case {
    Environment,
    Five,
    OptOut,
    Empty,
    Custom,
    AuthenticatedReject,
    VariantAuthenticatedReject,
    Cancel,
    Deadline,
    Transport,
    Malformed,
    Mixed,
}
#[tokio::test]
async fn five_initial_variants_reuse_matched_initial_connection_and_exact_wire() {
    run(Case::Five).await;
}
#[tokio::test]
async fn initial_rejection_without_opt_in_never_falls_back() {
    run(Case::OptOut).await;
}
#[tokio::test]
async fn explicit_empty_rejection_codes_do_not_fall_back() {
    run(Case::Empty).await;
}
#[tokio::test]
async fn configured_initial_status_retries_only_the_initial_phase() {
    run(Case::Custom).await;
}
#[tokio::test]
async fn authenticated_403_is_terminal_even_with_fallback_enabled() {
    run(Case::AuthenticatedReject).await;
}
#[tokio::test]
async fn challenge_after_variant_still_makes_authenticated_rejection_terminal() {
    run(Case::VariantAuthenticatedReject).await;
}
#[tokio::test]
async fn cancellation_after_rejection_does_not_reopen_a_variant() {
    run(Case::Cancel).await;
}
#[tokio::test]
async fn candidate_deadline_does_not_reopen_a_variant() {
    run(Case::Deadline).await;
}
#[tokio::test]
async fn enabled_initial_eof_may_retry_only_after_real_retirement() {
    run(Case::Transport).await;
}
#[tokio::test]
async fn malformed_initial_response_never_downgrades_to_none() {
    run(Case::Malformed).await;
}
#[tokio::test]
async fn existing_environment_opt_in_enables_initial_variants_without_config_mutation() {
    run(Case::Environment).await;
}
#[tokio::test]
async fn rejection_then_eof_preserves_variant_and_cseq_progress() {
    run(Case::Mixed).await;
}
fn header(text: &str, name: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix(name).map(str::trim))
        .unwrap()
        .into()
}
async fn read(
    reader: &mut SipStreamReader<
        tokio::io::ReadHalf<proletarius_vowifi::netstack_tcp::ManagedTcpStream>,
    >,
) -> String {
    let Some(SipStreamItem::Message(bytes)) = reader.next().await.unwrap() else {
        panic!("REGISTER")
    };
    String::from_utf8(bytes.to_vec()).unwrap()
}
fn response(request: &str, status: u16) -> Vec<u8> {
    let extra = if status == 401 {
        "WWW-Authenticate: Digest realm=\"ims.mnc033.mcc234.3gppnetwork.org\",nonce=\"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=\",algorithm=AKAv1-MD5\r\n"
    } else {
        ""
    };
    format!("SIP/2.0 {status} Fixture\r\nCall-ID: {}\r\nCSeq: {}\r\n{extra}Expires: 3600\r\nContent-Length: 0\r\n\r\n",header(request,"Call-ID:"),header(request,"CSeq:")).into_bytes()
}
async fn run(case: Case) {
    tokio::time::timeout(Duration::from_secs(6), exchange(case))
        .await
        .expect("fallback fixture outer deadline");
}
async fn exchange(case: Case) {
    let config = |ip: &str| TunnelConfiguration {
        ipv4_addresses: vec![ip.parse().unwrap()],
        ..Default::default()
    };
    let local_net = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
    let remote_net = TunnelNetwork::new(&config("10.20.30.9")).unwrap();
    let (local_wake_tx, local_wake) = watch::channel(0u64);
    let (remote_wake_tx, remote_wake) = watch::channel(0u64);
    let (local_tcp, local_registry) = TcpSocketRegistry::new(local_wake_tx.clone());
    let (remote_tcp, remote_registry) = TcpSocketRegistry::new(remote_wake_tx);
    let (security, _runtime) = SecurityNetworkRuntime::new(
        |_: &mut smoltcp::iface::Interface, _: &mut smoltcp::iface::SocketSet<'static>| {
            Ok::<(), Infallible>(())
        },
        local_wake_tx,
    );
    let (local_out, remote_in) = mpsc::channel(64);
    let (remote_out, local_in) = mpsc::channel(64);
    let (network_stop, network_stopped) = watch::channel(false);
    let local = tokio::spawn(local_net.run_managed(
        smoltcp::iface::SocketSet::new(vec![]),
        local_in,
        local_out,
        network_stopped.clone(),
        local_wake,
        local_registry,
    ));
    let remote = tokio::spawn(remote_net.run_managed(
        smoltcp::iface::SocketSet::new(vec![]),
        remote_in,
        remote_out,
        network_stopped,
        remote_wake,
        remote_registry,
    ));
    let (stop, signal) = watch::channel(false);
    let peer_stop = stop.clone();
    let (ready, ready_rx) = oneshot::channel();
    let (done, done_rx) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let total = match case {
            Case::Five => 5,
            Case::Mixed => 3,
            Case::Custom
            | Case::Transport
            | Case::Environment
            | Case::VariantAuthenticatedReject => 2,
            _ => 1,
        };
        let mut first_id = None;
        let mut previous_via = None;
        let mut previous_from = None;
        let mut ready = Some(ready);
        let mut connection = None;
        let mut accepted = 0;
        for index in 0..total {
            if connection.is_none() {
                let listener = remote_tcp
                    .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
                    .await
                    .unwrap();
                if let Some(ready) = ready.take() {
                    ready.send(()).unwrap();
                }
                let stream = listener.accept().await.unwrap();
                accepted += 1;
                let (read_half, write) = tokio::io::split(stream);
                connection = Some((SipStreamReader::new(read_half), write));
            }
            let (mut reader, mut write) = connection.take().unwrap();
            let request = read(&mut reader).await;
            assert!(request.starts_with("REGISTER "));
            assert_eq!(header(&request, "CSeq:"), format!("{} REGISTER", index + 1));
            if let Some(id) = first_id.as_ref() {
                assert_eq!(&header(&request, "Call-ID:"), id);
            } else {
                first_id = Some(header(&request, "Call-ID:"));
            }
            if let Some(via) = previous_via.replace(header(&request, "Via:")) {
                assert_ne!(header(&request, "Via:"), via);
            }
            if let Some(from) = previous_from.replace(header(&request, "From:")) {
                assert_ne!(header(&request, "From:"), from);
            }
            if matches!(case, Case::Five) {
                match index {
                    1 => assert!(header(&request, "Authorization:").starts_with("Digest uri=")),
                    2 => {
                        assert!(header(&request, "Authorization:").starts_with("Digest username="))
                    }
                    3 => assert!(request.contains("response=\"00000000000000000000000000000000\"")),
                    4 => assert!(!request.contains("Authorization:")),
                    _ => {}
                }
                assert_eq!(request.contains("P-Access-Network-Info:"), index != 4);
                assert_eq!(request.contains("Cellular-Network-Info:"), index != 4);
            }
            if matches!(case, Case::Deadline) {
                assert!(reader.next().await.unwrap().is_none());
            } else if (matches!(case, Case::Transport) && index == 0)
                || (matches!(case, Case::Mixed) && index == 1)
            { /* real EOF without a response */
            } else {
                let status = match case {
                    Case::Five if index < 4 => 403,
                    Case::Environment | Case::Mixed | Case::VariantAuthenticatedReject
                        if index == 0 =>
                    {
                        403
                    }
                    Case::OptOut | Case::Empty | Case::Cancel => 403,
                    Case::Custom if index == 0 => 488,
                    Case::AuthenticatedReject | Case::VariantAuthenticatedReject => 401,
                    _ => 200,
                };
                let bytes = if matches!(case, Case::Malformed) {
                    b"SIP/2.0 invalid Broken\r\nContent-Length: 0\r\n\r\n".to_vec()
                } else {
                    response(&request, status)
                };
                write.write_all(&bytes).await.unwrap();
                if matches!(case, Case::Five) && index == 0 {
                    write.write_all(b"MESSAGE sip:fixture@example.invalid SIP/2.0\r\nCall-ID: early-message\r\nCSeq: 1 MESSAGE\r\nContent-Length: 0\r\n\r\n\r\n").await.unwrap();
                }
                if matches!(case, Case::Cancel) {
                    peer_stop.send(true).unwrap();
                }
                if matches!(
                    case,
                    Case::AuthenticatedReject | Case::VariantAuthenticatedReject
                ) && index + 1 == total
                {
                    let auth = read(&mut reader).await;
                    assert_eq!(header(&auth, "CSeq:"), format!("{} REGISTER", index + 2));
                    assert!(auth.contains("Authorization: Digest "));
                    assert!(!auth.contains("response=\"\""));
                    write.write_all(&response(&auth, 403)).await.unwrap();
                }
                if index + 1 < total {
                    connection = Some((reader, write));
                    continue;
                }
                assert!(reader.next().await.unwrap().is_none());
            }
            reader.into_inner().unsplit(write).close().await.unwrap();
        }
        assert_eq!(
            accepted,
            if matches!(case, Case::Transport | Case::Mixed) {
                2
            } else {
                1
            }
        );
        // Terminal cases must not open another variant on the same tuple.
        if !matches!(
            case,
            Case::Five | Case::Custom | Case::Transport | Case::Environment | Case::Mixed
        ) {
            let mut next = remote_tcp
                .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(80), next.accept_cancel_safe())
                    .await
                    .is_err()
            );
            next.close().await.unwrap();
        }
        done.send(()).unwrap();
    });
    ready_rx.await.unwrap();
    let extra = match case {
        Case::OptOut | Case::Environment => "",
        Case::Empty => {
            "      enable_initial_reject_fallback: true\n      register_policy:\n        initial_reject_fallback_status_codes: []\n"
        }
        Case::Custom => {
            "      enable_initial_reject_fallback: true\n      register_policy:\n        initial_reject_fallback_status_codes: [488]\n"
        }
        _ => "      enable_initial_reject_fallback: true\n",
    };
    let p = super::tests::profile(extra);
    let d = p.resolve_device_identity(Some("356938035643809")).unwrap();
    let runtime = NativeImsRuntime {
        imsi: "234330000000001",
        mcc: "234",
        mnc: "33",
        local_ip: "10.0.0.2".parse().unwrap(),
        pcscf: "10.20.30.9:6060".parse().unwrap(),
        call_id: "initial-variant-fixture",
        from_tag: "first",
        via_branch: "z9hG4bKfirst",
        cell_id: None,
        isim_private_identity: None,
        isim_public_identity: None,
    };
    let sub = p.resolve_subscriber_identity(runtime).unwrap();
    let candidates = [Candidate {
        local_ip: runtime.local_ip,
        registrar: runtime.pcscf,
        transport: runtime.pcscf,
    }];
    let (events, _received) = mpsc::channel(16);
    let budget = if matches!(case, Case::Deadline) {
        Duration::from_millis(80)
    } else {
        Duration::from_secs(4)
    };
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let result = super::super::finish_owned_registration(
        register_with_environment(
            RegistrationServices::new(&local_tcp, &security, &events),
            &p,
            &d,
            &sub,
            runtime,
            NativeSecurityEndpoints {
                spi_c: 1,
                spi_s: 2,
                port_c: 5064,
                port_s: 5063,
            },
            None,
            &candidates,
            tokio::time::Instant::now() + budget,
            signal,
            move |_, _| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    Ok(SimAnswer::Success {
                        res: Zeroizing::new(vec![7; 8]),
                        ck: Zeroizing::new([2; 16]),
                        ik: Zeroizing::new([3; 16]),
                    })
                }
            },
            matches!(case, Case::Environment),
        ),
        std::future::pending(),
        stop,
    )
    .await;
    if matches!(
        case,
        Case::Five | Case::Custom | Case::Transport | Case::Environment | Case::Mixed
    ) {
        let mut session = result.unwrap_or_else(|_| panic!("fallback did not register"));
        assert_eq!(session.signaling_peer(), runtime.pcscf);
        if matches!(case, Case::Five) {
            let event = session.next_event().await.unwrap().unwrap().into_item();
            let SipStreamItem::Message(message) = event else {
                panic!("retained early MESSAGE")
            };
            assert!(String::from_utf8_lossy(&message).contains("Call-ID: early-message"));
            assert!(matches!(
                session.next_event().await.unwrap().unwrap().into_item(),
                SipStreamItem::KeepAliveLine
            ));
        }
        session.shutdown().await.unwrap();
    } else {
        let Err(NativeImsRegistrationError::Registration(failure)) = result else {
            panic!("terminal case registered")
        };
        assert!(failure.registration().cleanup().is_clean());
        if matches!(
            case,
            Case::AuthenticatedReject | Case::VariantAuthenticatedReject
        ) {
            assert!(matches!(
                failure.registration().cause(),
                RegistrationError::Sip {
                    stage: RegistrationStage::PlainAuthenticatedRegister,
                    source: SipError::RegistrationRejected(403)
                } | RegistrationError::Transport {
                    stage: RegistrationStage::PlainAuthenticatedRegister,
                    source: RegisterWaitError::Sip(SipError::RegistrationRejected(403))
                }
            ));
        }
    }
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        usize::from(matches!(
            case,
            Case::AuthenticatedReject | Case::VariantAuthenticatedReject
        ))
    );
    done_rx.await.unwrap();
    peer.await.unwrap();
    network_stop.send(true).unwrap();
    local.await.unwrap().unwrap();
    remote.await.unwrap().unwrap();
}
