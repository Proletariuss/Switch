#![cfg(target_os = "macos")]

use super::*;
use proletarius_ike::cert_auth::SignatureAlgorithm;
use security_framework::certificate::SecCertificate;
use std::{
    io::Write,
    process::{Command, Stdio},
};
use zeroize::Zeroizing;
const ROOT: &[u8] = include_bytes!("fixtures/root.der");
const RSA: &[u8] = include_bytes!("fixtures/rsa.der");
const EC: &[u8] = include_bytes!("fixtures/ec.der");
fn sign(data: &[u8], ec: bool) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/native_ike_certificate/fixtures")
        .join(if ec { "ec.test-pem" } else { "rsa.test-pem" });
    let mut child = Command::new("/usr/bin/openssl")
        .args(["dgst", "-sha256", "-sign"])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(data).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    result.stdout
}
fn request(name: &str, ec: bool) -> CertificateAuthRequest {
    let signed_octets =
        Zeroizing::new(b"captured RealMessage2 | NonceI | prf(SK_pr, IDr)".to_vec());
    CertificateAuthRequest {
        certificates: vec![if ec { EC } else { RSA }.to_vec(), ROOT.to_vec()],
        expected_name: name.into(),
        algorithm: if ec {
            SignatureAlgorithm::EcdsaSha256
        } else {
            SignatureAlgorithm::RsaPkcs1Sha256
        },
        signature: sign(&signed_octets, ec),
        signed_octets,
    }
}
#[test]
fn system_ipsec_policy_trust_name_signature_and_ip_literals() {
    let root = SecCertificate::from_der(ROOT).unwrap();
    for ec in [false, true] {
        for name in ["epdg.test", "127.0.0.1", "::1"] {
            assert_eq!(
                platform::evaluate(request(name, ec), Some(std::slice::from_ref(&root))),
                Ok(()),
                "name={name}, ec={ec}"
            );
        }
        assert_eq!(
            platform::evaluate(request("wrong.test", ec), Some(std::slice::from_ref(&root))),
            Err(CertificateAuthError::UntrustedCertificate)
        );
        assert_eq!(
            platform::evaluate(request("127.0.0.2", ec), Some(std::slice::from_ref(&root))),
            Err(CertificateAuthError::UntrustedCertificate)
        );
        assert_eq!(
            evaluate(request("epdg.test", ec)),
            Err(CertificateAuthError::UntrustedCertificate)
        );
        let mut tampered = request("epdg.test", ec);
        tampered.signature[0] ^= 0x80;
        assert_eq!(
            platform::evaluate(tampered, Some(std::slice::from_ref(&root))),
            Err(CertificateAuthError::InvalidSignature)
        );
        let mut transcript = request("epdg.test", ec);
        transcript.signed_octets[0] ^= 1;
        assert_eq!(
            platform::evaluate(transcript, Some(std::slice::from_ref(&root))),
            Err(CertificateAuthError::InvalidSignature)
        );
    }
}
#[test]
fn system_boundary_rejects_invalid_der_and_oversize_without_trust() {
    let mut invalid = request("epdg.test", false);
    invalid.certificates[0] = vec![0x30, 1, 0];
    assert_eq!(
        evaluate(invalid),
        Err(CertificateAuthError::InvalidCertificate)
    );
    let mut oversized = request("epdg.test", false);
    oversized.certificates[0] = vec![0; 32 * 1024 + 1];
    assert_eq!(
        evaluate(oversized),
        Err(CertificateAuthError::ChainTooLarge)
    );
}

#[derive(Debug, Default)]
struct WireProgress {
    client: std::sync::Mutex<&'static str>,
    peer: std::sync::Mutex<&'static str>,
    trust: std::sync::Mutex<&'static str>,
}

struct HeldTrust {
    started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    finish: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}
struct TestTrust {
    trusted: bool,
    progress: Arc<WireProgress>,
    hold: Option<Arc<HeldTrust>>,
    workers: Arc<tokio::sync::Semaphore>,
}
impl PeerCertificateVerifier for TestTrust {
    fn verify(&self, request: CertificateAuthRequest) -> CertificateVerifyFuture {
        let trusted = self.trusted;
        let progress = self.progress.clone();
        let hold = self.hold.clone();
        let workers = self.workers.clone();
        Box::pin(async move {
            *progress.trust.lock().unwrap() = "waiting for worker admission";
            run_bounded(request, workers, move |request| {
                *progress.trust.lock().unwrap() = "system trust/signature evaluation";
                let evaluated = if trusted {
                    let root = SecCertificate::from_der(ROOT).unwrap();
                    platform::evaluate(request, Some(&[root]))
                } else {
                    evaluate(request)
                };
                *progress.trust.lock().unwrap() = "system evaluation returned";
                if let Some(hold) = hold {
                    // Execute real system trust/signature validation first. The
                    // held closure still owns its actual-worker permit, and its
                    // result has not reached the IKE/EAP continuation. This keeps
                    // the 2s exit proof about worker return, rather than timing
                    // unrelated system trust service contention after cancellation.
                    assert_eq!(evaluated, Ok(()), "actual platform verification");
                    let started = hold.started.lock().unwrap().take().unwrap();
                    let finish = hold.finish.lock().unwrap().take().unwrap();
                    *progress.trust.lock().unwrap() = "waiting for explicit worker release";
                    started.send(()).unwrap();
                    finish.recv().unwrap();
                }
                *progress.trust.lock().unwrap() = "actual worker returning";
                evaluated
            })
            .await
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum WireCase {
    Valid,
    EcdsaValid,
    CancelBeforeWorkerReturn,
    WrongName,
    Untrusted,
    SignatureTampered,
    TranscriptTampered,
    InvalidAlg,
    Oversize,
    MissingCert,
    MissingAuth,
    MissingIdr,
    DuplicateIdr,
    DuplicateAuth,
    DuplicateCert,
    RepeatedCert,
    WrongFinalMsk,
    EapOnly,
}
fn frame(kind: u8, body: Vec<u8>) -> proletarius_ike::PayloadFrame {
    proletarius_ike::PayloadFrame {
        kind,
        critical: false,
        body,
    }
}
async fn wire_case(case: WireCase, required: bool) {
    use proletarius_ike::{
        ExchangeType, IkeHeader, Modp2048KeyPair, ProposalProtocol, build_native_proposals,
        compute_eap_auth, decode_payload_chain, derive_ike_sa_key_material, encode_payload_chain,
        sk::{open_ike_sk_gcm, seal_ike_sk_gcm},
    };
    use proletarius_vowifi::{
        EapHandshakeError, EpdgDatagramTransport, IkeAuthError, NativeTunnelOptions,
        NativeTunnelSettings, ProxyMode, RequestedIpStack, SimAkaResult, TunnelStartupError,
    };
    use std::{
        net::{IpAddr, Ipv4Addr},
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    // Entire exchange, including actual MODP SA_INIT and final MSK AUTH, stays
    // on loopback. No DNS, system keychain writes, SIM, or public network.
    // This is a fixture deadlock watchdog, not a protocol deadline. Full-suite
    // diagnostics observed SecTrust still evaluating WrongName after 8 seconds.
    // Allow the system service to finish under concurrent tests; actual worker
    // drain, rejected EAP traffic and production exchange deadlines stay intact.
    let progress = Arc::new(WireProgress::default());
    tokio::time::timeout(Duration::from_secs(30),async {
        *progress.client.lock().unwrap() = "binding loopback fixture";
        let peer=tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST,0)).await.unwrap();
        socket2::SockRef::from(&peer).set_send_buffer_size(128 * 1024).unwrap();
        let port=peer.local_addr().unwrap().port();
        let expected_name=if case==WireCase::WrongName {"wrong.test"}else{"epdg.test"};
        let (trust_started, trust_ready) = tokio::sync::oneshot::channel();
        let (trust_finish, trust_complete) = std::sync::mpsc::channel();
        let (trust_drained, trust_drained_rx) = tokio::sync::oneshot::channel();
        let (client_finished, client_finished_rx) = tokio::sync::oneshot::channel();
        let workers = Arc::new(tokio::sync::Semaphore::new(2));
        let verifier=Arc::new(TestTrust{trusted:case!=WireCase::Untrusted, progress:progress.clone(),
            hold: (case==WireCase::CancelBeforeWorkerReturn).then(||Arc::new(HeldTrust {started:std::sync::Mutex::new(Some(trust_started)),finish:std::sync::Mutex::new(Some(trust_complete))})),
            workers:workers.clone()});
        let policy=if required {PeerCertificatePolicy::new(expected_name,verifier)}else{PeerCertificatePolicy::optional(expected_name,verifier)}.unwrap();
        let peer_progress = progress.clone();
        let mut server=tokio::spawn(async move {
            *peer_progress.peer.lock().unwrap() = "waiting for SA_INIT";
            let mut trust_drained_rx = Some(trust_drained_rx);
            let mut client_finished_rx = Some(client_finished_rx);
            let mut buf=vec![0;65536];
            let (n,source)=peer.recv_from(&mut buf).await.unwrap();
            let raw_init=buf[..n].to_vec();
            let (mut header,body)=IkeHeader::decode(&raw_init).unwrap();
            assert_eq!(header.exchange,ExchangeType::IkeSaInit);
            let frames=decode_payload_chain(header.next_payload,body).unwrap();
            assert_eq!(proletarius_ike::cert_auth::signature_hashes(&raw_init).unwrap(),vec![2,3,4]);
            let ni=frames.iter().find(|f|f.kind==40).unwrap().body.clone();
            let ke=frames.iter().find(|f|f.kind==34).unwrap();
            let dh=Modp2048KeyPair::generate().unwrap();
            let shared=dh.shared_secret(&ke.body[4..]).unwrap();
            let nr=vec![8;32];
            let keys=derive_ike_sa_key_material(5,&ni,&nr,&shared,header.initiator_spi,22,20,0).unwrap();
            let ike=build_native_proposals(&["aes128gcm16-prfsha256-modp2048".into()],ProposalProtocol::Ike,&[]).unwrap();
            let (first,body)=encode_payload_chain(&[frame(33,ike),frame(34,[&[0,14,0,0][..],dh.public_key()].concat()),frame(40,nr.clone()),proletarius_ike::cert_auth::signature_hash_offer()]).unwrap();
            header.responder_spi=22;header.flags=IkeHeader::RESPONSE_FLAG;header.next_payload=first;
            let init_response=header.encode(&body).unwrap();
            peer.send_to(&init_response,source).await.unwrap();
            *peer_progress.peer.lock().unwrap() = "signing certificate AUTH fixture";
            let idr=[&[2,0,0,0][..],b"epdg.test"].concat();
            let signed=proletarius_ike::auth_signed_octets(5,&init_response,&ni,&keys.sk_pr,&idr).unwrap();
            let ec = case == WireCase::EcdsaValid;
            let mut sig=sign(&signed,ec);
            if case==WireCase::SignatureTampered {sig[0]^=1;}
            if case==WireCase::TranscriptTampered {sig=sign(b"different initial response",false);}
            let mut alg=if ec {vec![0x30,0x0a,0x06,0x08,0x2a,0x86,0x48,0xce,0x3d,0x04,0x03,2]} else {vec![0x30,0x0d,0x06,0x09,0x2a,0x86,0x48,0x86,0xf7,0x0d,0x01,0x01,11,5,0]};
            if case==WireCase::InvalidAlg {alg[12]=10;}
            let signature_auth=frame(39,[&[14,0,0,0,alg.len() as u8][..],&alg,&sig].concat());
            let cert=frame(37,[&[4][..],if ec {EC}else{RSA}].concat());
            let identity=b"nai@example";
            let golden="014d004417010000010500001111111111111111111111111111111102050000222222222222222222222222222222220b05000019a5cd42f0c8250e9e34edb24815c777";
            let challenge:Vec<u8>=(0..golden.len()).step_by(2).map(|i|u8::from_str_radix(&golden[i..i+2],16).unwrap()).collect();
            let aka=proletarius_ike::eap_aka::answer_eap_aka_challenge(&challenge,identity,&[0x11,0x22,0x33,0x44,0x55,0x66,0x77,0x88],&[0x31;16],&[0x42;16]).unwrap();
            let mut selectors=Vec::new();
            let mut idi=Vec::new();
            for (round,packet) in [vec![1,9,0,5,1],challenge,vec![3,77,0,4]].into_iter().enumerate() {
                *peer_progress.peer.lock().unwrap() = "waiting for IKE_AUTH request";
                let (n,from)=peer.recv_from(&mut buf).await.unwrap();assert_eq!(from,source);
                let (mut auth_header,request)=open_ike_sk_gcm(&buf[..n],20,&keys.sk_ei).unwrap();
                if round==0 {
                    idi=request.iter().find(|f|f.kind==35).unwrap().body.clone();
                    assert_eq!(idi,[&[3,0,0,0][..],identity].concat());
                    assert_eq!(request.iter().any(|f|f.kind==38&&f.body==[4]),required);
                    assert_eq!(request.iter().any(|f|f.kind==41&&f.body==[0,0,0x40,0x21]),!required);
                    selectors=request.iter().filter(|f|matches!(f.kind,44|45)).cloned().collect::<Vec<_>>();
                }
                if round==1 {assert_eq!(&request[0].body[5..],identity);}
                if round==2 {assert_eq!(request[0].body,aka.packet);}
                let mut response=vec![frame(48,packet)];
                if round==0 { response.insert(0,frame(36,idr.clone())); }
                if round==0 && case!=WireCase::EapOnly {
                    response.push(cert.clone());response.push(frame(37,[&[4][..],ROOT].concat()));response.push(signature_auth.clone());
                    match case {
                        WireCase::MissingIdr=>response.retain(|f|f.kind!=36),
                        WireCase::MissingCert=>response.retain(|f|f.kind!=37),
                        WireCase::MissingAuth=>response.retain(|f|f.kind!=39),
                        WireCase::DuplicateIdr=>response.push(frame(36,idr.clone())),
                        WireCase::DuplicateAuth=>response.push(signature_auth.clone()),
                        WireCase::DuplicateCert=>response.push(cert.clone()),
                        WireCase::Oversize=>response.iter_mut().find(|f|f.kind==37).unwrap().body=vec![4;32*1024+2],
                        _=>{}
                    }
                }
                if round==1 && case==WireCase::RepeatedCert {response.push(cert.clone());}
                auth_header.flags=IkeHeader::RESPONSE_FLAG;
                let wire=seal_ike_sk_gcm(auth_header,20,&keys.sk_er,&response).unwrap();
                peer.send_to(&wire,source).await.unwrap();
                if (round==0 && !matches!(case,WireCase::Valid|WireCase::EcdsaValid|WireCase::RepeatedCert|WireCase::WrongFinalMsk|WireCase::EapOnly)) || (round==1&&case==WireCase::RepeatedCert) {
                    if case == WireCase::CancelBeforeWorkerReturn {
                        // Keep the actual peer alive until the cancelled real
                        // verifier has returned; observe late-result leakage too.
                        trust_drained_rx.take().unwrap().await.unwrap();
                    }
                    // Observe the whole client lifetime, not an arbitrary quiet
                    // interval while system trust may still be evaluating.
                    *peer_progress.peer.lock().unwrap() = "waiting for client completion after rejection";
                    client_finished_rx.take().unwrap().await.unwrap();
                    // Query the kernel directly: Tokio's cached readiness may
                    // lag the client completion signal within the same poll.
                    let peer = peer.into_std().unwrap();
                    let unexpected = peer.recv_from(&mut buf);
                    assert!(
                        matches!(&unexpected, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                        "{case:?}: EAP response escaped certificate rejection: {unexpected:?}"
                    );
                    *peer_progress.peer.lock().unwrap() = "rejection peer completed";
                    return;
                }
            }
            let (n,_)=peer.recv_from(&mut buf).await.unwrap();
            let (mut auth_header,request)=open_ike_sk_gcm(&buf[..n],20,&keys.sk_ei).unwrap();
            assert_eq!(request[0].kind,39);assert_eq!(request[0].body[0],2);
            assert_eq!(&request[0].body[4..],compute_eap_auth(5,&aka.msk,&raw_init,&nr,&keys.sk_pi,&idi).unwrap().as_slice());
            let mut final_auth=compute_eap_auth(5,&aka.msk,&init_response,&ni,&keys.sk_pr,&idr).unwrap();
            if case==WireCase::WrongFinalMsk {final_auth[0]^=1;}
            let child=build_native_proposals(&["aes128gcm16".into()],ProposalProtocol::Esp,&[0,0,0,9]).unwrap();
            let mut response=vec![frame(39,[&[2,0,0,0][..],final_auth.as_slice()].concat()),frame(33,child),frame(47,vec![2,0,0,0,0,1,0,4,10,0,0,2])];
            response.extend(selectors);
            auth_header.flags=IkeHeader::RESPONSE_FLAG;
            let wire=seal_ike_sk_gcm(auth_header,20,&keys.sk_er,&response).unwrap();
            peer.send_to(&wire,source).await.unwrap();
        });
        let transport=EpdgDatagramTransport::open(ProxyMode::Direct(IpAddr::V4(Ipv4Addr::LOCALHOST)),"epdg.test",port,None).await.unwrap();
        let calls=Arc::new(AtomicUsize::new(0));let observed=calls.clone();
        let (stop_tx,stop)=tokio::sync::watch::channel(false);
        let _keep_stop=stop_tx.clone();
        let cancel_task=(case==WireCase::CancelBeforeWorkerReturn).then(||tokio::spawn(async move {trust_ready.await.unwrap();stop_tx.send(true).unwrap();}));
        *progress.client.lock().unwrap() = "native startup in flight";
        let ike_proposals = ["aes128gcm16-prfsha256-modp2048".into()];
        let esp_proposals = ["aes128gcm16".into()];
        let startup=proletarius_vowifi::start_configured_native_tunnel_with_options(transport,NativeTunnelSettings {
            identity:b"nai@example",apn:"ims",device_identity_imei:None,ike_proposals:&ike_proposals,esp_proposals:&esp_proposals,ip_stack:RequestedIpStack::Ipv4,
            checkcode_mode:proletarius_ike::eap_aka::AkaCheckcodeMode::Minimal,initial_contact:false,retain_eap_direct_material:false,
        },NativeTunnelOptions{peer_certificate:Some(policy),disable_switch_to_natt_port:true,..Default::default()},move |_| {observed.fetch_add(1,Ordering::SeqCst);async {Ok::<_,()>(SimAkaResult::Success {res:vec![0x11,0x22,0x33,0x44,0x55,0x66,0x77,0x88],ck:[0x31;16],ik:[0x42;16]})}},stop);
        tokio::pin!(startup);
        let mut peer_joined = false;
        let result = tokio::select! {
            result = &mut startup => result,
            joined = &mut server => {
                joined.unwrap_or_else(|error| panic!("{case:?}: peer task failed before client completion: {error}; {progress:?}"));
                peer_joined = true;
                startup.await
            }
        };
        *progress.client.lock().unwrap() = "native startup returned";
        if let Some(cancel_task) = cancel_task {
            cancel_task.await.unwrap();
            assert_eq!(workers.available_permits(),1,"cancel dropped a live trust worker permit");
            trust_finish.send(()).unwrap();
            let proof=tokio::time::timeout(Duration::from_secs(2),workers.clone().acquire_many_owned(2)).await.unwrap().unwrap();
            drop(proof);
            trust_drained.send(()).unwrap();
        }
        // Startup has returned, and cancellation (when exercised) has also
        // drained the actual trust worker. No EAP continuation remains live.
        let _ = client_finished.send(());
        *progress.client.lock().unwrap() = "joining peer after worker cleanup";
        if !peer_joined {
            server.await.unwrap_or_else(|error| panic!("{case:?}: peer task failed: {error}; {progress:?}"));
        }
        *progress.client.lock().unwrap() = "checking terminal result";
        match case {
            WireCase::Valid|WireCase::EcdsaValid|WireCase::EapOnly=>{assert!(result.is_ok(),"{case:?}: {:?}",result.err());assert_eq!(calls.load(Ordering::SeqCst),1);}
            WireCase::CancelBeforeWorkerReturn=>{assert!(matches!(result,Err(TunnelStartupError::Cancelled)));assert_eq!(calls.load(Ordering::SeqCst),0);}
            WireCase::WrongFinalMsk=>{assert!(matches!(result,Err(TunnelStartupError::Auth(IkeAuthError::ResponderAuthenticationFailed))));assert_eq!(calls.load(Ordering::SeqCst),1);}
            WireCase::DuplicateIdr=>{assert!(matches!(result,Err(TunnelStartupError::Eap(EapHandshakeError::Transport(IkeAuthError::ResponderAuthenticationFailed)))));assert_eq!(calls.load(Ordering::SeqCst),0);}
            _=>{assert!(matches!(result,Err(TunnelStartupError::Eap(EapHandshakeError::Certificate(_)))),"{case:?}: {:?}",result.err());assert_eq!(calls.load(Ordering::SeqCst),0);}
        }
    }).await.unwrap_or_else(|error| panic!("{case:?}: wire case deadline: {error}; {progress:?}"));
}
#[tokio::test]
async fn actual_sa_init_certificate_signature_then_eap_and_final_msk_auth() {
    wire_case(WireCase::Valid, true).await;
    wire_case(WireCase::EcdsaValid, true).await;
    wire_case(WireCase::WrongFinalMsk, true).await;
}
#[tokio::test]
async fn optional_signature_auth_is_verified_and_eap_only_remains_compatible() {
    wire_case(WireCase::Valid, false).await;
    wire_case(WireCase::WrongName, false).await;
    wire_case(WireCase::EapOnly, false).await;
}
#[tokio::test]
async fn certificate_wire_failures_never_submit_eap_or_sim_auth() {
    for case in [
        WireCase::WrongName,
        WireCase::Untrusted,
        WireCase::SignatureTampered,
        WireCase::TranscriptTampered,
        WireCase::InvalidAlg,
        WireCase::Oversize,
        WireCase::MissingCert,
        WireCase::MissingAuth,
        WireCase::MissingIdr,
        WireCase::DuplicateIdr,
        WireCase::DuplicateAuth,
        WireCase::DuplicateCert,
        WireCase::RepeatedCert,
    ] {
        wire_case(case, true).await;
    }
}

#[tokio::test]
async fn cancelled_verification_retains_actual_worker_budget_until_real_completion() {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    let workers = Arc::new(tokio::sync::Semaphore::new(1));
    let (started, ready) = tokio::sync::oneshot::channel();
    let (finish, complete) = std::sync::mpsc::channel();
    let actual = tokio::spawn(run_bounded(
        request("epdg.test", false),
        workers.clone(),
        move |_| {
            started.send(()).unwrap();
            complete.recv().unwrap();
            Ok(())
        },
    ));
    ready.await.unwrap();
    actual.abort();
    assert!(actual.await.unwrap_err().is_cancelled());
    assert_eq!(
        workers.available_permits(),
        0,
        "cancelled waiter released live worker budget"
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    let queued = tokio::spawn(run_bounded(
        request("epdg.test", false),
        workers.clone(),
        move |_| {
            called.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    ));
    tokio::task::yield_now().await;
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(workers.available_permits(), 0);
    finish.send(()).unwrap();
    let permit = tokio::time::timeout(Duration::from_secs(2), workers.clone().acquire_owned())
        .await
        .unwrap()
        .unwrap();
    drop(permit);
    assert_eq!(workers.available_permits(), 1);
}

#[tokio::test]
async fn cancelling_after_actual_trust_before_worker_return_never_sends_eap_or_releases_permit() {
    wire_case(WireCase::CancelBeforeWorkerReturn, true).await;
}
