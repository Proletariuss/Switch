//! Full native startup over a local SOCKS relay, with no real SIM or DNS.
use proletarius_ike::{
    ExchangeType, IkeHeader, Modp2048KeyPair, PayloadFrame, ProposalProtocol,
    build_native_proposals, compute_eap_auth, decode_payload_chain, derive_ike_sa_key_material,
    derive_initial_child_sa_keys,
    eap_aka::answer_eap_aka_challenge,
    encode_payload_chain,
    esp::{EspReceiver, EspSender},
    sk::{SkPayloadError, open_ike_sk_cbc, open_ike_sk_gcm, seal_ike_sk_cbc, seal_ike_sk_gcm},
};
use proletarius_vowifi::*;
use std::net::SocketAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UdpSocket},
    sync::watch,
};
use zeroize::Zeroizing;

#[path = "rekey_exchange/peer.rs"]
mod rekey;
pub(super) use rekey::Client as RekeyClient;

#[derive(Clone, Copy)]
enum NativeSuite {
    CtExcel,
}
impl NativeSuite {
    fn ike_proposal(self) -> &'static str {
        "aes128-sha256-prfsha256-modp2048"
    }
    fn esp_proposal(self) -> &'static str {
        "aes128-sha256"
    }
    fn encryption_transform(self) -> u16 {
        12
    }
    fn integrity_transform(self) -> Option<u16> {
        Some(12)
    }
}

fn open_ike_sk(
    suite: NativeSuite,
    packet: &[u8],
    encryption_key: &[u8],
    integrity_key: &[u8],
) -> Result<(IkeHeader, Vec<PayloadFrame>), SkPayloadError> {
    match suite.integrity_transform() {
        None => open_ike_sk_gcm(packet, suite.encryption_transform(), encryption_key),
        Some(integrity) => open_ike_sk_cbc(
            packet,
            suite.encryption_transform(),
            integrity,
            encryption_key,
            integrity_key,
        ),
    }
}

fn seal_ike_sk(
    suite: NativeSuite,
    header: IkeHeader,
    encryption_key: &[u8],
    integrity_key: &[u8],
    frames: &[PayloadFrame],
) -> Result<Vec<u8>, SkPayloadError> {
    match suite.integrity_transform() {
        None => seal_ike_sk_gcm(header, suite.encryption_transform(), encryption_key, frames),
        Some(integrity) => seal_ike_sk_cbc(
            header,
            suite.encryption_transform(),
            integrity,
            encryption_key,
            integrity_key,
            frames,
        ),
    }
}

fn esp_receiver(
    suite: NativeSuite,
    spi: u32,
    encryption_key: Vec<u8>,
    integrity_key: Vec<u8>,
) -> EspReceiver {
    match suite.integrity_transform() {
        None => {
            assert!(integrity_key.is_empty());
            EspReceiver::new(spi, Zeroizing::new(encryption_key)).unwrap()
        }
        Some(integrity) => EspReceiver::new_cbc(
            spi,
            Zeroizing::new(encryption_key),
            integrity,
            Zeroizing::new(integrity_key),
        )
        .unwrap(),
    }
}

fn esp_sender(
    suite: NativeSuite,
    spi: u32,
    encryption_key: Vec<u8>,
    integrity_key: Vec<u8>,
) -> EspSender {
    match suite.integrity_transform() {
        None => {
            assert!(integrity_key.is_empty());
            EspSender::new(spi, Zeroizing::new(encryption_key)).unwrap()
        }
        Some(integrity) => EspSender::new_cbc(
            spi,
            Zeroizing::new(encryption_key),
            integrity,
            Zeroizing::new(integrity_key),
        )
        .unwrap(),
    }
}

fn frame(kind: u8, body: Vec<u8>) -> PayloadFrame {
    PayloadFrame {
        kind,
        critical: false,
        body,
    }
}
fn ike_offer(suite: NativeSuite) -> Vec<u8> {
    build_native_proposals(&[suite.ike_proposal().into()], ProposalProtocol::Ike, &[]).unwrap()
}
fn esp_offer(suite: NativeSuite, spi: u32) -> PayloadFrame {
    let body = build_native_proposals(
        &[suite.esp_proposal().into()],
        ProposalProtocol::Esp,
        &spi.to_be_bytes(),
    )
    .unwrap();
    frame(33, body)
}
fn ts(kind: u8) -> PayloadFrame {
    frame(
        kind,
        vec![
            1, 0, 0, 0, 7, 0, 0, 16, 0, 0, 255, 255, 0, 0, 0, 0, 255, 255, 255, 255,
        ],
    )
}
async fn recv(udp: &UdpSocket, port: u16) -> (Vec<u8>, SocketAddr) {
    let mut buf = [0; 4096];
    let (n, source) = udp.recv_from(&mut buf).await.unwrap();
    let packet = decode_udp_datagram(&buf[..n]).unwrap();
    assert_eq!(packet.target, SocksTarget::Domain("epdg.invalid".into()));
    assert_eq!(packet.port, port);
    (packet.payload, source)
}
async fn send(udp: &UdpSocket, source: SocketAddr, port: u16, packet: &[u8]) {
    udp.send_to(
        &encode_udp_domain("epdg.invalid", port, packet).unwrap(),
        source,
    )
    .await
    .unwrap();
}

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::mpsc;
#[derive(Default)]
pub(super) struct Trace {
    pub(super) ike: AtomicUsize,
    pub(super) deletes: AtomicUsize,
    pub(super) registers: AtomicUsize,
    pub(super) stop_requested: AtomicBool,
    pub(super) planned_refresh_failure: AtomicBool,
}
pub(super) struct Peer {
    pub(super) proxy: SocketAddr,
    pub(super) trace: Arc<Trace>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
impl Peer {
    pub(super) async fn new() -> Self {
        Self::configured(None, None, None, None).await
    }
    pub(super) async fn with_sms(
        root: std::path::PathBuf,
        subscription: String,
    ) -> (Self, super::sms_peer::Client) {
        let (server, client) = super::sms_peer::new(root, subscription);
        (
            Self::configured(Some(server), None, None, None).await,
            client,
        )
    }
    pub(super) async fn with_sms_go_exit(
        root: std::path::PathBuf,
        subscription: String,
    ) -> (Self, super::sms_peer::Client) {
        let (mut server, client) = super::sms_peer::new(root, subscription);
        server.recover_go_exit = true;
        (
            Self::configured(Some(server), None, None, None).await,
            client,
        )
    }
    pub(super) async fn with_multi_sms(
        root: std::path::PathBuf,
        store: crate::multistandby_store::Store,
        profile_id: String,
    ) -> (Self, super::sms_peer::Client) {
        let (mut server, client) = super::sms_peer::new(root, String::new());
        server.multi = Some((store, profile_id));
        (
            Self::configured(Some(server), None, None, None).await,
            client,
        )
    }
    pub(super) async fn with_module_business(
        root: std::path::PathBuf,
        subscription: String,
    ) -> (Self, super::sms_peer::Client, super::call_peer::Client) {
        let (mut sms, sms_client) = super::sms_peer::new(root, subscription);
        sms.target = "tel:+447802002606".into();
        let (calls, call_client) = super::call_peer::new();
        (
            Self::configured(Some(sms), Some(calls), None, None).await,
            sms_client,
            call_client,
        )
    }
    pub(super) async fn with_calls() -> (Self, super::call_peer::Client) {
        let (server, client) = super::call_peer::new();
        (
            Self::configured(None, Some(server), None, None).await,
            client,
        )
    }
    pub(super) async fn with_refresh_calls(
        mode: super::call_peer::RefreshMode,
    ) -> (Self, super::call_peer::Client) {
        let (mut server, client) = super::call_peer::new();
        server.refresh = Some(mode);
        let peer = Self::configured(None, Some(server), None, None).await;
        peer.trace
            .planned_refresh_failure
            .store(true, Ordering::SeqCst);
        (peer, client)
    }
    pub(super) async fn with_renewal(server: super::renewal_peer::Server) -> Self {
        Self::configured(None, None, Some(server), None).await
    }
    pub(super) async fn with_rekey_sms(
        root: std::path::PathBuf,
        subscription: String,
    ) -> (Self, super::sms_peer::Client, RekeyClient) {
        let (sms, client) = super::sms_peer::new(root, subscription);
        let (server, rekey) = rekey::new();
        (
            Self::configured(Some(sms), None, None, Some(server)).await,
            client,
            rekey,
        )
    }
    pub(super) async fn with_rekey_calls() -> (Self, super::call_peer::Client, RekeyClient) {
        let (calls, client) = super::call_peer::new();
        let (server, rekey) = rekey::new();
        (
            Self::configured(None, Some(calls), None, Some(server)).await,
            client,
            rekey,
        )
    }
    pub(super) async fn with_heartbeat_policy(
        policy: super::nwait_heartbeat_policy::Server,
    ) -> Self {
        Self::configured_with_policy(None, None, None, None, Some(policy)).await
    }
    async fn configured(
        sms: Option<super::sms_peer::Server>,
        calls: Option<super::call_peer::Server>,
        renewal: Option<super::renewal_peer::Server>,
        rekey: Option<mpsc::Receiver<rekey::Command>>,
    ) -> Self {
        Self::configured_with_policy(sms, calls, renewal, rekey, None).await
    }
    pub(super) async fn with_multi_profile_oracle(
        oracle: Arc<super::manager_multi_profile::Oracle>,
    ) -> Self {
        Self::configured_with_oracle(None, None, None, None, None, Some(oracle)).await
    }
    async fn configured_with_policy(
        sms: Option<super::sms_peer::Server>,
        calls: Option<super::call_peer::Server>,
        renewal: Option<super::renewal_peer::Server>,
        rekey: Option<mpsc::Receiver<rekey::Command>>,
        policy: Option<super::nwait_heartbeat_policy::Server>,
    ) -> Self {
        Self::configured_with_oracle(sms, calls, renewal, rekey, policy, None).await
    }
    async fn configured_with_oracle(
        sms: Option<super::sms_peer::Server>,
        calls: Option<super::call_peer::Server>,
        renewal: Option<super::renewal_peer::Server>,
        rekey: Option<mpsc::Receiver<rekey::Command>>,
        policy: Option<super::nwait_heartbeat_policy::Server>,
        oracle: Option<Arc<super::manager_multi_profile::Oracle>>,
    ) -> Self {
        let policy = Arc::new(std::sync::Mutex::new(policy));
        let rekey = Arc::new(std::sync::Mutex::new(rekey));
        let sms = Arc::new(std::sync::Mutex::new(sms));
        let calls = Arc::new(std::sync::Mutex::new(calls));
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = tcp.local_addr().unwrap();
        let trace = Arc::new(Trace::default());
        let server_trace = trace.clone();
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut sessions = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    pair = tcp.accept() => {
                        let (control, _) = pair.unwrap();
                        let trace = server_trace.clone();
                        let sms = sms.clone();
                        let calls = calls.clone();
                        let renewal = renewal.clone();
                        let rekey = rekey.clone();
                        let policy = policy.clone();
                        let oracle = oracle.clone();
                        sessions.spawn(async move { connection(control, trace, sms, calls, renewal, rekey, policy, oracle).await });
                    }
                    _ = stopped.changed() => break,
                }
            }
            while let Some(result) = sessions.join_next().await {
                result.unwrap();
            }
        });
        Self {
            proxy,
            trace,
            stop,
            task,
        }
    }
    pub(super) async fn finish(self) {
        self.stop.send_replace(true);
        self.task.await.unwrap();
    }
}
// Build a real RFC4187 challenge for the production NAI, not the historical
// nai@example vector. The responder independently computes its request MAC.
fn challenge(identity: &[u8]) -> Vec<u8> {
    challenge_with_material(identity, &[0x11; 16], &[0x22; 16], &[0x31; 16], &[0x42; 16])
}
fn challenge_with_material(
    identity: &[u8],
    rand: &[u8; 16],
    autn: &[u8; 16],
    ck: &[u8; 16],
    ik: &[u8; 16],
) -> Vec<u8> {
    use sha1::digest::core_api::Block;
    use sha1::{Digest, Sha1, Sha1Core, compress};
    let mut mk_input = identity.to_vec();
    mk_input.extend(ik);
    mk_input.extend(ck);
    let mut xkey: [u8; 20] = Sha1::digest(&mk_input).into();
    let mut keymat = Vec::new();
    for _ in 0..2 {
        let mut block = Block::<Sha1Core>::default();
        block[..20].copy_from_slice(&xkey);
        let mut state = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
        compress(&mut state, &[block]);
        let digest: Vec<u8> = state.iter().flat_map(|v| v.to_be_bytes()).collect();
        keymat.extend(&digest);
        let mut carry = 1u16;
        for i in (0..20).rev() {
            let sum = xkey[i] as u16 + digest[i] as u16 + carry;
            xkey[i] = sum as u8;
            carry = sum >> 8;
        }
    }
    let mut packet = vec![1, 77, 0, 68, 23, 1, 0, 0, 1, 5, 0, 0];
    packet.extend(rand);
    packet.extend([2, 5, 0, 0]);
    packet.extend(autn);
    packet.extend([11, 5, 0, 0]);
    packet.extend([0; 16]);
    let mut pad = [0u8; 64];
    pad[..16].copy_from_slice(&keymat[16..32]);
    let inner: Vec<u8> = pad
        .iter()
        .map(|v| v ^ 0x36)
        .chain(packet.iter().copied())
        .collect();
    let digest = Sha1::digest(&inner);
    let outer: Vec<u8> = pad.iter().map(|v| v ^ 0x5c).chain(digest).collect();
    packet[52..68].copy_from_slice(&Sha1::digest(&outer)[..16]);
    packet
}
async fn connection(
    mut control: tokio::net::TcpStream,
    trace: Arc<Trace>,
    sms: Arc<std::sync::Mutex<Option<super::sms_peer::Server>>>,
    calls: Arc<std::sync::Mutex<Option<super::call_peer::Server>>>,
    renewal: Option<super::renewal_peer::Server>,
    rekey: Arc<std::sync::Mutex<Option<mpsc::Receiver<rekey::Command>>>>,
    policy: Arc<std::sync::Mutex<Option<super::nwait_heartbeat_policy::Server>>>,
    oracle: Option<Arc<super::manager_multi_profile::Oracle>>,
) {
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut greeting = [0; 3];
    control.read_exact(&mut greeting).await.unwrap();
    assert_eq!(greeting, [5, 1, 0]);
    control.write_all(&[5, 0]).await.unwrap();
    let mut request = [0; 10];
    if let Err(error) = control.read_exact(&mut request).await {
        // Production performs a SOCKS availability probe before UDP ASSOCIATE.
        // EOF here precedes every IKE packet and is not a tunnel completion.
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
        return;
    }
    assert_eq!(&request[..3], &[5, 3, 0]);
    let mut reply = vec![5, 0, 0, 1, 127, 0, 0, 1];
    reply.extend(udp.local_addr().unwrap().port().to_be_bytes());
    control.write_all(&reply).await.unwrap();
    let mut eof = [0; 1];
    let initial = tokio::select! {
        value=recv(&udp,500)=>value,
        value=control.read(&mut eof)=> { assert_eq!(value.unwrap(),0);return; },
    };
    let (raw_init, source) = initial;
    trace.ike.fetch_add(1, Ordering::SeqCst);
    let suite = NativeSuite::CtExcel;
    let (mut header, body) = IkeHeader::decode(&raw_init).unwrap();
    let frames = decode_payload_chain(header.next_payload, body).unwrap();
    assert_eq!(
        frames.iter().find(|f| f.kind == 33).unwrap().body,
        ike_offer(suite)
    );
    let ni = frames.iter().find(|f| f.kind == 40).unwrap().body.clone();
    let ke = &frames.iter().find(|f| f.kind == 34).unwrap().body;
    let dh = Modp2048KeyPair::generate().unwrap();
    let shared = dh.shared_secret(&ke[4..]).unwrap();
    let nr = vec![8; 32];
    let mut ke = vec![0, 14, 0, 0];
    ke.extend(dh.public_key());
    header.responder_spi = 22;
    header.flags = IkeHeader::RESPONSE_FLAG;
    let (next, body) = encode_payload_chain(&[
        frame(33, ike_offer(suite)),
        frame(34, ke),
        frame(40, nr.clone()),
    ])
    .unwrap();
    header.next_payload = next;
    let raw_reply = header.encode(&body).unwrap();
    let keys =
        derive_ike_sa_key_material(5, &ni, &nr, &shared, header.initiator_spi, 22, 16, 32).unwrap();
    send(&udp, source, 500, &raw_reply).await;
    let idr = [&[2, 0, 0, 0][..], b"epdg.invalid"].concat();
    let mut idi = Vec::new();
    let mut identity = Vec::new();
    let mut spi = 0;
    let mut answer = None;
    let mut association = None;
    for round in 0..3 {
        let (wire, from) = tokio::select! {
            value = recv(&udp, 4500) => value,
            value = control.read(&mut eof) => {
                assert_eq!(value.unwrap(), 0);
                assert!(trace.stop_requested.load(Ordering::SeqCst), "IKE handshake closed without planned stop");
                return;
            }
        };
        assert_eq!(from, source);
        let (mut h, f) = open_ike_sk(suite, &wire[4..], &keys.sk_ei, &keys.sk_ai).unwrap();
        assert_eq!(h.message_id, round + 1);
        if round == 0 {
            idi = f.iter().find(|v| v.kind == 35).unwrap().body.clone();
            identity = idi[4..].to_vec();
            if let Some(oracle) = oracle.as_ref() {
                // The decrypted IDi binds this connection exactly once; accept
                // ordering and the currently enabled physical profile are irrelevant.
                association = Some(oracle.bind_exact_idi(&identity, header.initiator_spi));
            } else {
                let expected_imsi = policy
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|peer| peer.imsi.clone())
                    .unwrap_or_else(|| "234330123456789".into());
                assert!(String::from_utf8_lossy(&identity).contains(&expected_imsi));
            }
            let offered = proletarius_ike::decode_sa_proposals(
                &f.iter().find(|v| v.kind == 33).unwrap().body,
            )
            .unwrap();
            spi = u32::from_be_bytes(offered[0].spi.as_slice().try_into().unwrap());
        }
        let eap = match round {
            0 => vec![1, 9, 0, 5, 1],
            1 => {
                assert_eq!(&f[0].body[5..], identity);
                let (challenge, res, ck, ik) = if let Some(bound) = association.as_ref() {
                    assert_eq!(
                        &f[0].body[5..],
                        bound.profile.nai().as_bytes(),
                        "exact independent EAP identity"
                    );
                    let material = bound.profile.ike();
                    (
                        challenge_with_material(
                            &identity,
                            &[0x11; 16],
                            &[0x22; 16],
                            &material.ck,
                            &material.ik,
                        ),
                        material.res,
                        material.ck,
                        material.ik,
                    )
                } else {
                    (
                        challenge(&identity),
                        [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
                        [0x31; 16],
                        [0x42; 16],
                    )
                };
                answer =
                    Some(answer_eap_aka_challenge(&challenge, &identity, &res, &ck, &ik).unwrap());
                challenge
            }
            _ => {
                assert_eq!(f[0].body, answer.as_ref().unwrap().packet);
                if let Some(bound) = association.as_ref() {
                    bound.facts.eap_verified.fetch_add(1, Ordering::SeqCst);
                }
                vec![3, 77, 0, 4]
            }
        };
        h.flags = IkeHeader::RESPONSE_FLAG;
        let reply = seal_ike_sk(
            suite,
            h,
            &keys.sk_er,
            &keys.sk_ar,
            &[frame(36, idr.clone()), frame(48, eap)],
        )
        .unwrap();
        send(&udp, source, 4500, &proletarius_ike::encode_natt(&reply)).await;
    }
    let (wire, _) = recv(&udp, 4500).await;
    let (mut h, f) = open_ike_sk(suite, &wire[4..], &keys.sk_ei, &keys.sk_ai).unwrap();
    let msk = &answer.as_ref().unwrap().msk;
    assert_eq!(
        &f[0].body[4..],
        &*compute_eap_auth(5, msk, &raw_init, &nr, &keys.sk_pi, &idi).unwrap()
    );
    let mut auth = vec![2, 0, 0, 0];
    auth.extend(&*compute_eap_auth(5, msk, &raw_reply, &ni, &keys.sk_pr, &idr).unwrap());
    h.flags = IkeHeader::RESPONSE_FLAG;
    let cp = vec![
        2, 0, 0, 0, 0, 1, 0, 4, 10, 0, 0, 2, 0, 20, 0, 4, 10, 20, 30, 9,
    ];
    let reply = seal_ike_sk(
        suite,
        h,
        &keys.sk_er,
        &keys.sk_ar,
        &[
            frame(39, auth),
            esp_offer(suite, 2),
            ts(44),
            ts(45),
            frame(47, cp),
        ],
    )
    .unwrap();
    send(&udp, source, 4500, &proletarius_ike::encode_natt(&reply)).await;
    let mut child = derive_initial_child_sa_keys(5, &keys.sk_d, &ni, &nr, 16, 32).unwrap();
    let mut rx = esp_receiver(
        suite,
        2,
        std::mem::take(&mut child.initiator_encryption),
        std::mem::take(&mut child.initiator_integrity),
    );
    let mut tx = esp_sender(
        suite,
        spi,
        std::mem::take(&mut child.responder_encryption),
        std::mem::take(&mut child.responder_integrity),
    );
    let (inbound, in_rx) = mpsc::channel(128);
    let (outbound, mut out_rx) = mpsc::channel(128);
    let (network_stop, stopped) = watch::channel(false);
    // This association alone may close signaling after our zero-lifetime
    // notice. A later association must start without that expectation.
    let planned_reauthentication = Arc::new(AtomicBool::new(false));
    let association_facts = association.as_ref().map(|bound| bound.facts.clone());
    let server = tokio::spawn(super::ims_peer::run(
        in_rx,
        outbound,
        stopped,
        trace.clone(),
        sms.lock().unwrap().take(),
        calls.lock().unwrap().take(),
        renewal.as_ref().map(|server| server.association()),
        planned_reauthentication.clone(),
        policy.lock().unwrap().take(),
        association,
    ));
    let mut rekey = rekey.lock().unwrap().take();
    let mut rekey_state = rekey::State::new(spi);
    let mut control_open = true;
    let mut delete_deadline = None;
    loop {
        let mut buf = [0; 65536];
        tokio::select! {
            command=async {if let Some(input)=&mut rekey {input.recv().await} else {std::future::pending().await}}=>{
                let command=command.unwrap();
                let closes_association=matches!(&command,rekey::Command::Lifetime{seconds:0,..});
                let packet=rekey_state.command(command,header.initiator_spi,&keys);
                if closes_association {
                    // Publish before send: the client can process the actual
                    // notice and close TCP before this task resumes. Sending
                    // still fails normally; this never substitutes for DELETE.
                    planned_reauthentication.store(true,Ordering::SeqCst);
                }
                send(&udp,source,4500,&proletarius_ike::encode_natt(&packet)).await;
            }
            packet=udp.recv_from(&mut buf)=>{
                let (n,_)=packet.unwrap();let packet=decode_udp_datagram(&buf[..n]).unwrap();
                if packet.payload==[0xff] {continue;}
                if packet.payload.starts_with(&[0;4]) {
                    let (mut h,f)=open_ike_sk(suite,&packet.payload[4..],&keys.sk_ei,&keys.sk_ai).unwrap();
                    if rekey.is_some() && rekey_state.packet(&packet.payload[4..],&h,&f,&keys,&udp,source,&mut rx,&mut tx).await {continue;}
                    if rekey.is_some() {
                        assert_eq!(h.exchange,ExchangeType::Informational);
                        assert!(!h.is_response());
                        assert!(f.is_empty() || (f.len()==1 && f[0].kind==42 && f[0].body==[1,0,0,0]),"unexpected protected control during rekey fixture");
                    }
                    let deleting=f.iter().find(|v|v.kind==42);
                    if let Some(delete) = deleting {
                        assert_eq!(delete.body, [1, 0, 0, 0], "expected actual IKE-SA DELETE");
                        // Publish the verified receive before sending its ACK:
                        // the client may finish stop immediately after that send.
                        trace.deletes.fetch_add(1, Ordering::SeqCst);
                        if let Some(facts) = association_facts.as_ref() { facts.deletes.fetch_add(1, Ordering::SeqCst); }
                    }
                    h.flags=IkeHeader::RESPONSE_FLAG;
                    let reply=seal_ike_sk(suite,h,&keys.sk_er,&keys.sk_ar,&[]).unwrap();send(&udp,source,4500,&proletarius_ike::encode_natt(&reply)).await;
                    if deleting.is_some() {break;}
                } else {
                    let ip=rx.open(&packet.payload).unwrap();inbound.send(ip).await.unwrap();
                }
            }
            ip=out_rx.recv()=>{
                let ip=ip.unwrap();send(&udp,source,4500,&tx.seal(&ip).unwrap()).await;
            }
            value=control.read(&mut eof), if control_open=>{
                assert_eq!(value.unwrap(),0);
                assert!(trace.stop_requested.load(Ordering::SeqCst) || trace.planned_refresh_failure.load(Ordering::SeqCst) || renewal.is_some() || rekey.is_some(), "SOCKS owner disappeared before explicit test stop");
                // TCP EOF can outrun the UDP packet the tunnel just submitted.
                // Keep the simulated carrier alive to receive and verify DELETE.
                control_open = false;
                delete_deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(2));
            }
            _=async {
                if let Some(deadline) = delete_deadline { tokio::time::sleep_until(deadline).await; }
                else { std::future::pending::<()>().await; }
            }=>panic!("IKE owner closed SOCKS without delivering encrypted DELETE"),
        }
    }
    network_stop.send_replace(true);
    drop(inbound);
    server.await.unwrap();
    if let Some(facts) = association_facts {
        facts.joined.fetch_add(1, Ordering::SeqCst);
    }
}
