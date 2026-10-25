//! A real protected P-CSCF behind the authenticated IKE peer.
use super::ike_peer::Trace;
use proletarius_ims::{
    aka_exchange::SecurityKeys,
    security_offer::parse_security_server,
    security_policy::{LocalEndpoints, SecurityPolicy},
    stream::{SipStreamItem, SipStreamReader},
};
use proletarius_sip::{Message, Parser, Request};
use proletarius_vowifi::{
    TunnelConfiguration, ims_network_runtime::ImsNetworkRuntime, netstack_runtime::TunnelNetwork,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, watch},
};
use zeroize::Zeroizing;
fn request(wire: &[u8]) -> Request {
    match Parser::default().parse(wire, false).unwrap().0 {
        Message::Request(r) => r,
        _ => panic!("expected SIP request"),
    }
}
pub(super) fn response(r: &Request, status: u16, extra: &str, body: &str) -> Vec<u8> {
    let headers = ["Via", "From", "To", "Call-ID", "CSeq"]
        .iter()
        .map(|name| format!("{name}: {}\r\n", r.data.headers.get(name).unwrap().value()))
        .collect::<String>();
    format!(
        "SIP/2.0 {status} Test\r\n{headers}{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}
pub(super) async fn run(
    input: mpsc::Receiver<Zeroizing<Vec<u8>>>,
    output: mpsc::Sender<Zeroizing<Vec<u8>>>,
    stopped: watch::Receiver<bool>,
    trace: Arc<Trace>,
    mut sms: Option<super::sms_peer::Server>,
    mut calls: Option<super::call_peer::Server>,
    mut renewal: Option<super::renewal_peer::Association>,
    planned_reauthentication: Arc<AtomicBool>,
    mut heartbeat: Option<super::nwait_heartbeat_policy::Server>,
    mut multi: Option<super::manager_multi_profile::BoundOracle>,
) {
    let network = TunnelNetwork::new(&TunnelConfiguration {
        ipv4_addresses: vec!["10.20.30.9".parse().unwrap()],
        ..Default::default()
    })
    .unwrap();
    let (wake, awake) = watch::channel(0);
    let (tcp, security, udp, runtime) = ImsNetworkRuntime::new_with_udp(wake);
    let mut network_stop = stopped.clone();
    let runner = tokio::spawn(async move {
        network
            .run_managed(
                smoltcp::iface::SocketSet::new(vec![]),
                input,
                output,
                stopped,
                awake,
                runtime,
            )
            .await
    });
    if let Some(calls) = calls.as_mut() {
        calls.start_media(udp, network_stop.clone()).await;
    }
    let listener = tcp.listen(5060, None).await.unwrap();
    let plain = listener.accept().await.unwrap();
    let (read, mut write) = tokio::io::split(plain);
    let mut reader = SipStreamReader::new(read);
    let Some(SipStreamItem::Message(wire)) = reader.next().await.unwrap() else {
        panic!("missing initial REGISTER")
    };
    let initial = request(&wire);
    assert_eq!(initial.method.as_str(), "REGISTER");
    let client =
        parse_security_server(initial.data.headers.get("Security-Client").unwrap().value())
            .unwrap()
            .remove(0);
    let policy = SecurityPolicy::aes_sha1(
        "10.20.30.9".parse().unwrap(),
        "10.0.0.2".parse().unwrap(),
        LocalEndpoints {
            spi_c: 111,
            spi_s: 222,
            port_c: 6050,
            port_s: 6060,
        },
        &client,
        multi
            .as_ref()
            .map(|bound| {
                let keys = bound.profile.ims();
                SecurityKeys {
                    ck: Zeroizing::new(keys.ck),
                    ik: Zeroizing::new(keys.ik),
                }
            })
            .unwrap_or_else(|| SecurityKeys {
                ck: Zeroizing::new([2; 16]),
                ik: Zeroizing::new([3; 16]),
            }),
    )
    .unwrap();
    let lease = security.install(policy).await.unwrap();
    let protected = tcp.listen(6060, None).await.unwrap();
    let nonce = multi
        .as_ref()
        .map(|bound| bound.nonce(1))
        .unwrap_or_else(|| "01".repeat(32));
    let domain = multi
        .as_ref()
        .map(|bound| bound.profile.domain().to_owned())
        .or_else(|| heartbeat.as_ref().map(|p| p.domain.clone()))
        .unwrap_or_else(|| "ims.mnc033.mcc234.3gppnetwork.org".into());
    let challenge = format!(
        "WWW-Authenticate: Digest realm=\"{domain}\",nonce=\"{nonce}\",algorithm=AKAv1-MD5\r\nSecurity-Server: ipsec-3gpp;q=0.98;alg=hmac-sha-1-96;ealg=aes-cbc;prot=esp;mod=trans;spi-c=111;spi-s=222;port-c=6050;port-s=6060\r\n"
    );
    write
        .write_all(&response(&initial, 401, &challenge, ""))
        .await
        .unwrap();
    write.flush().await.unwrap();
    assert!(reader.next().await.unwrap().is_none());
    reader.into_inner().unsplit(write).close().await.unwrap();
    let stream = protected.accept().await.unwrap();
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = SipStreamReader::new(read);
    let Some(SipStreamItem::Message(wire)) = reader.next().await.unwrap() else {
        panic!("missing protected REGISTER")
    };
    let authenticated = request(&wire);
    assert_eq!(authenticated.method.as_str(), "REGISTER");
    assert!(
        authenticated
            .data
            .headers
            .get("Security-Verify")
            .unwrap()
            .value()
            .contains("aes-cbc")
    );
    assert!(
        authenticated
            .data
            .headers
            .get("Authorization")
            .unwrap()
            .value()
            .contains("integrity-protected=yes")
    );
    if let Some(calls) = calls.as_mut() {
        calls.registration_seed(&authenticated);
    }
    trace.registers.fetch_add(1, Ordering::SeqCst);
    if let Some(bound) = multi.as_mut() {
        bound.registered(&authenticated);
    }
    if let Some(policy) = heartbeat.as_mut() {
        policy.registered(&authenticated);
    }
    let expiry = multi
        .as_ref()
        .map(|bound| bound.profile.expiry())
        .or_else(|| heartbeat.as_ref().map(|p| p.expiry))
        .unwrap_or_else(|| {
            renewal.as_ref().map_or_else(
                || {
                    if calls.as_ref().is_some_and(|c| c.refresh.is_some()) {
                        4
                    } else {
                        3600
                    }
                },
                |peer| peer.expiry,
            )
        });
    let associated = multi
        .as_ref()
        .map(|bound| bound.profile.public_identity())
        .unwrap_or_else(|| format!("sip:+447700900123@{domain}"));
    let extra = format!(
        "Expires: {expiry}\r\nP-Associated-URI: <{associated}>\r\nService-Route: <sip:10.20.30.9:6060;lr>\r\n"
    );
    let mut registered_wire = Vec::new();
    if let Some(sms) = sms.as_mut() {
        // A real SMS precedes the final REGISTER response in the same batch.
        // Only the retained startup FIFO can deliver it to the native consumer.
        registered_wire.extend(sms.delivery());
    }
    if let Some(policy) = heartbeat.as_ref() {
        registered_wire.extend(policy.delivery());
    }
    let rejected = renewal.as_ref().is_some_and(|peer| peer.reject_register());
    registered_wire.extend(response(
        &authenticated,
        if rejected { 503 } else { 200 },
        &extra,
        "",
    ));
    write.write_all(&registered_wire).await.unwrap();
    write.flush().await.unwrap();
    if let Some(peer) = renewal.as_ref() {
        peer.events
            .send(if rejected {
                super::renewal_peer::Event::RegisterRejected(peer.generation)
            } else {
                super::renewal_peer::Event::Registered(peer.generation)
            })
            .unwrap();
    }
    let mut keepalive_lines = 0;
    loop {
        tokio::select! {
            frame=reader.next()=>{
                match frame.unwrap() {
                    Some(SipStreamItem::KeepAliveLine) => {
                        // RFC5626 CRLF-CRLF ping consists of two framed lines.
                        // A single CRLF is a pong, never socket EOF.
                        keepalive_lines += 1;
                        if keepalive_lines == 2 {
                            write.write_all(b"\r\n").await.unwrap();
                            write.flush().await.unwrap();
                            keepalive_lines = 0;
                        }
                    }
                    Some(SipStreamItem::Message(wire)) => {
                        keepalive_lines = 0;
                        let message = Parser::default().parse(&wire, false).unwrap().0;
                        if let Some(bound) = multi.as_mut() {
                            bound.observe(message, &mut write).await;
                            continue;
                        }
                        if let Some(policy) = heartbeat.as_mut() {
                            policy.observe(message, &mut write).await;
                            continue;
                        }
                        if let Message::Request(request) = &message && request.method.as_str() == "OPTIONS" {
                            write.write_all(&response(request, 200, "", "")).await.unwrap();
                            write.flush().await.unwrap();
                            continue;
                        }
                        let is_sms = match &message {
                            Message::Request(v) => v.method.as_str() == "MESSAGE",
                            Message::Response(v) => v.data.headers.get("CSeq").is_some_and(|h| h.value().ends_with(" MESSAGE")),
                        };
                        if let Some(sms) = sms.as_mut().filter(|_| is_sms) {
                            sms.observe(message, &mut write).await;
                        } else if let Some(calls) = calls.as_mut() {
                            calls.observe(message, &mut write).await;
                        } else if let Some(peer) = renewal.as_mut() {
                            peer.observe(message, &mut write).await;
                        } else {
                            let Message::Request(r) = message else { panic!("unexpected SIP response in startup-only fixture") };
                            assert_eq!(r.method.as_str(), "REGISTER", "unexpected application work in startup/stop fixture");
                            write.write_all(&response(&r,200,&extra,"")).await.unwrap();
                            write.flush().await.unwrap();
                        }
                    }
                    None => {
                        if let Some(bound) = multi.as_ref() {
                            assert!(bound.facts.stop_requested.load(Ordering::SeqCst), "this exact association was not stopped");
                        }
                        assert!(trace.stop_requested.load(Ordering::SeqCst) || trace.planned_refresh_failure.load(Ordering::SeqCst) || renewal.is_some() || planned_reauthentication.load(Ordering::SeqCst), "client closed protected signaling before explicit test stop or this association's zero-lifetime notice");
                        // Retain the accepted socket and security lease until
                        // authenticated IKE DELETE confirms explicit stop.
                        network_stop.wait_for(|value| *value).await.unwrap();
                        break;
                    }
                }
            },
            command=async { sms.as_mut().unwrap().commands.recv().await }, if sms.is_some()=>{
                match command.expect("SMS control must survive until actual tunnel stop") {
                    super::sms_peer::Command::Duplicate => {
                        write.write_all(&sms.as_ref().unwrap().delivery()).await.unwrap();
                        write.flush().await.unwrap();
                    }
                }
            },
            command=async { calls.as_mut().unwrap().commands.recv().await }, if calls.is_some()=>{
                calls.as_mut().unwrap().command(command.expect("call control remains alive"), &mut write).await;
            },
            command=async { renewal.as_mut().unwrap().commands.recv().await }, if renewal.is_some()=>{
                if renewal.as_mut().unwrap().command(command.expect("renewal control remains alive"), &mut write).await { break; }
            },
            command=async { heartbeat.as_mut().unwrap().commands.recv().await }, if heartbeat.is_some()=>{
                if heartbeat.as_mut().unwrap().command(command.expect("heartbeat peer command owner"), &mut write).await { break; }
            },
            command=async { multi.as_mut().unwrap().commands.recv().await }, if multi.is_some()=>{
                multi.as_mut().unwrap().command(command.expect("multi-profile command owner"), &mut write).await;
            },
            changed=network_stop.changed()=>{
                changed.unwrap();
                assert!(*network_stop.borrow(), "simulated tunnel stopped without DELETE");
                break;
            },
        }
    }
    // Stop may retire the remote IP runtime after the authenticated DELETE;
    // socket errors here are cleanup-only and never count as message ACKs.
    let _ = reader.into_inner().unsplit(write).close().await;
    if let Some(peer) = renewal.as_ref() {
        peer.events
            .send(super::renewal_peer::Event::SignalingClosed(peer.generation))
            .unwrap();
        network_stop.wait_for(|value| *value).await.unwrap();
    }
    if heartbeat.is_some() {
        // CloseSignaling is a real TCP EOF; retain security/network ownership
        // until the client acknowledges retirement with its encrypted DELETE.
        network_stop.wait_for(|value| *value).await.unwrap();
    }
    if multi.is_some() {
        network_stop.wait_for(|value| *value).await.unwrap();
    }
    if let Some(calls) = calls {
        calls.finish().await;
    }
    let _ = lease.clear().await;
    tokio::time::timeout(Duration::from_secs(2), runner)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let Some(bound) = multi {
        bound.facts.ims_joined.fetch_add(1, Ordering::SeqCst);
    }
}
