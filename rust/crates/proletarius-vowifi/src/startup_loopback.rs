//! Full native startup over a local SOCKS relay, with no real SIM or DNS.
use crate::*;
use proletarius_ike::{
    ExchangeType, IkeHeader, Modp1024KeyPair, Modp2048KeyPair, NatDetectionKind, PayloadFrame,
    ProposalProtocol, build_native_proposals, compute_eap_auth, decode_payload_chain,
    derive_ike_sa_key_material, derive_initial_child_sa_keys,
    eap_aka::{AkaCheckcodeMode, answer_eap_aka_challenge},
    encode_payload_chain,
    esp::{EspReceiver, EspSender},
    nat_detection_notify,
    sk::{SkPayloadError, open_ike_sk_cbc, open_ike_sk_gcm, seal_ike_sk_cbc, seal_ike_sk_gcm},
};
use smoltcp::{
    iface::{Config, Interface, SocketSet},
    socket::udp,
    time::Instant,
    wire::{HardwareAddress, IpCidr},
};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UdpSocket},
    sync::watch,
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug)]
enum NativeSuite {
    GcmBaseline,
    CtExcel,
    Giffgaff,
    Hutchison,
    Dito,
}

#[derive(Clone)]
struct NativeClientConfig {
    identity: Vec<u8>,
    apn: String,
    proposals: carrier::NativeAlgorithmProposals,
    ip_stack: RequestedIpStack,
    checkcode_mode: AkaCheckcodeMode,
}

impl NativeSuite {
    fn ike_proposal(self) -> &'static str {
        match self {
            Self::GcmBaseline => "aes128gcm16-prfsha256-modp2048",
            Self::CtExcel => "aes128-sha256-prfsha256-modp2048",
            Self::Hutchison => "aes256-sha256-prfsha256-modp2048",
            Self::Giffgaff => "aes256-sha512-prfsha512-modp2048",
            Self::Dito => "aes128-sha1-prfsha1-modp1024",
        }
    }

    fn esp_proposal(self) -> &'static str {
        match self {
            Self::GcmBaseline => "aes128gcm16",
            Self::CtExcel | Self::Hutchison => "aes128-sha256",
            Self::Giffgaff => "aes256-sha512",
            Self::Dito => "aes128-sha1",
        }
    }

    fn prf_id(self) -> u16 {
        match self {
            Self::GcmBaseline | Self::CtExcel | Self::Hutchison => 5,
            Self::Giffgaff => 7,
            Self::Dito => 2,
        }
    }

    fn encryption_transform(self) -> u16 {
        match self {
            Self::GcmBaseline => 20,
            Self::CtExcel | Self::Giffgaff | Self::Hutchison | Self::Dito => 12,
        }
    }

    fn integrity_transform(self) -> Option<u16> {
        match self {
            Self::GcmBaseline => None,
            Self::CtExcel | Self::Hutchison => Some(12),
            Self::Giffgaff => Some(14),
            Self::Dito => Some(2),
        }
    }

    fn ike_encryption_key_len(self) -> usize {
        match self {
            Self::GcmBaseline => 20,
            Self::CtExcel => 16,
            Self::Hutchison => 32,
            Self::Giffgaff => 32,
            Self::Dito => 16,
        }
    }

    fn esp_encryption_key_len(self) -> usize {
        match self {
            Self::GcmBaseline => 20,
            Self::CtExcel | Self::Hutchison => 16,
            Self::Giffgaff => 32,
            Self::Dito => 16,
        }
    }

    fn integrity_key_len(self) -> usize {
        match self {
            Self::GcmBaseline => 0,
            Self::CtExcel | Self::Hutchison => 32,
            Self::Giffgaff => 64,
            Self::Dito => 20,
        }
    }

    fn dh_group(self) -> u16 {
        match self {
            Self::Dito => 2,
            Self::GcmBaseline | Self::CtExcel | Self::Giffgaff | Self::Hutchison => 14,
        }
    }

    fn client_config(self) -> NativeClientConfig {
        let identity = match self {
            Self::GcmBaseline => {
                return NativeClientConfig {
                    identity: b"nai@example".to_vec(),
                    apn: "ims".into(),
                    proposals: carrier::NativeAlgorithmProposals {
                        ike: vec![self.ike_proposal().into()],
                        esp: vec![self.esp_proposal().into()],
                    },
                    ip_stack: RequestedIpStack::Ipv4,
                    checkcode_mode: AkaCheckcodeMode::Minimal,
                };
            }
            Self::CtExcel => carrier::SimIdentity {
                mcc: "234".into(),
                mnc: "33".into(),
                imsi: "234330123456789".into(),
                ..Default::default()
            },
            Self::Giffgaff => carrier::SimIdentity {
                mcc: "234".into(),
                mnc: "10".into(),
                imsi: "234100123456789".into(),
                gid1: "508fffff00".into(),
                ..Default::default()
            },
            Self::Hutchison => carrier::SimIdentity {
                mcc: "234".into(),
                mnc: "20".into(),
                imsi: "234200123456789".into(),
                ..Default::default()
            },
            Self::Dito => carrier::SimIdentity {
                mcc: "515".into(),
                mnc: "66".into(),
                imsi: "515660123456789".into(),
                ..Default::default()
            },
        };
        let profile = carrier::Registry::embedded()
            .unwrap()
            .resolve(&identity)
            .unwrap()
            .unwrap();
        let config = profile
            .native_tunnel_config(
                &identity,
                &carrier::NativeTunnelOverrides {
                    epdg: Some("epdg.invalid".into()),
                    ip_stack: Some("ipv4".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(config.epdg, "epdg.invalid");
        assert_eq!(config.proposals.ike, [self.ike_proposal()]);
        assert_eq!(config.proposals.esp, [self.esp_proposal()]);
        assert!(!config.identity().is_empty());
        NativeClientConfig {
            // The fixed challenge below has an independently pinned AT_MAC
            // for this synthetic identity. Carrier identity expansion is
            // validated separately in carrier tests.
            identity: b"nai@example".to_vec(),
            apn: config.apn,
            proposals: config.proposals,
            ip_stack: config.ip_stack,
            checkcode_mode: config.aka_challenge_mode,
        }
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

fn sip_socket() -> udp::Socket<'static> {
    let mut socket = udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 4096]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 4096]),
    );
    socket.bind(5060).unwrap();
    socket
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

// Responder DH work can outlast the original request's 300 ms retry clock.
// Accept only its exact frozen retry; a replacement attempt, a new message,
// or ESP must still fail the caller's protocol assertions. These cases allow
// two sends in total.
async fn recv_after_rekey_retries(
    udp: &UdpSocket,
    original: &[u8],
    cached_response: Option<&[u8]>,
    retries: &mut usize,
) -> (Vec<u8>, SocketAddr) {
    let original_header = IkeHeader::decode(&original[4..]).unwrap().0;
    loop {
        let (wire, source) = recv(udp, 4500).await;
        let same_request = wire.starts_with(&[0; 4])
            && IkeHeader::decode(&wire[4..]).is_ok_and(|(header, _)| {
                header.initiator_spi == original_header.initiator_spi
                    && header.responder_spi == original_header.responder_spi
                    && header.exchange == original_header.exchange
                    && header.message_id == original_header.message_id
            });
        if !same_request {
            return (wire, source);
        }
        assert_eq!(wire, original, "retry replaced the local rekey attempt");
        *retries += 1;
        assert!(*retries <= 1, "rekey exceeded the configured two sends");
        if let Some(response) = cached_response {
            send(udp, source, 4500, response).await;
        }
    }
}

#[tokio::test]
async fn native_startup_gcm_socks_natt_eap_auth_esp_dpd_and_delete() {
    native_network_case(NativeSuite::GcmBaseline, false).await;
    native_network_case(NativeSuite::GcmBaseline, true).await;
}

#[tokio::test]
async fn zero_auth_lifetime_in_final_auth_closes_before_first_esp() {
    native_network_case_options(NativeSuite::CtExcel, false, 22).await;
}

#[tokio::test]
async fn native_startup_ctexcel_cbc_socks_natt_eap_auth_esp_dpd_and_delete() {
    native_network_case(NativeSuite::CtExcel, false).await;
    native_network_case(NativeSuite::CtExcel, true).await;
}

#[tokio::test]
async fn native_startup_giffgaff_cbc_socks_natt_eap_auth_esp_dpd_and_delete() {
    native_network_case(NativeSuite::Giffgaff, false).await;
    native_network_case(NativeSuite::Giffgaff, true).await;
}

#[tokio::test]
async fn native_startup_hutchison_default_ike_socks_natt_eap_auth_esp_dpd_and_delete() {
    native_network_case(NativeSuite::Hutchison, false).await;
    native_network_case(NativeSuite::Hutchison, true).await;
}

#[tokio::test]
async fn native_startup_dito_group2_sha1_socks_natt_eap_auth_esp_dpd_and_delete() {
    native_network_case(NativeSuite::Dito, false).await;
    native_network_case(NativeSuite::Dito, true).await;
}

#[tokio::test]
async fn configured_final_cp_repeats_initial_request_on_real_encrypted_exchange() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 1).await;
}
#[tokio::test]
async fn configured_disabled_dpd_preserves_peer_control_and_delete() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 2).await;
}
#[tokio::test]
async fn configured_planned_reauthentication_sends_delete_before_terminal_reason() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 3).await;
}
#[tokio::test]
async fn configured_local_child_rekey_replaces_esp_and_deletes_old_sa() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 4).await;
}
#[tokio::test]
async fn configured_local_pfs_child_rekey_replaces_esp_and_deletes_old_sa() {
    native_network_case_options(NativeSuite::CtExcel, false, 5).await;
}
#[tokio::test]
async fn configured_peer_pfs_child_rekey_confirms_response_replaces_esp_and_retires_old_sa() {
    native_network_case_options(NativeSuite::CtExcel, false, 6).await;
}
#[tokio::test]
async fn child_rekey_lost_first_response_retries_identical_ciphertext_without_new_spi() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 7).await;
}
#[tokio::test]
async fn child_rekey_unknown_response_exits_without_fresh_attempt_or_esp_replacement() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 8).await;
}
#[tokio::test]
async fn ike_rekey_local_gcm_preserves_esp_and_resets_control_ids() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 9).await;
}
#[tokio::test]
async fn ike_rekey_local_cbc_preserves_esp_and_new_keys() {
    native_network_case_options(NativeSuite::CtExcel, false, 10).await;
}
#[tokio::test]
async fn ike_rekey_peer_initiated_flips_role_and_retires_old_control() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 11).await;
}
#[tokio::test]
async fn ike_rekey_lost_response_retries_identical_ciphertext() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 12).await;
}
#[tokio::test]
async fn ike_rekey_unknown_response_closes_without_fresh_attempt() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 13).await;
}
#[tokio::test]
async fn ike_rekey_stop_waits_exact_old_delete_before_new_delete() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 14).await;
}
#[tokio::test]
async fn ike_rekey_peer_delete_retry_replays_frozen_ack() {
    native_network_case_options(NativeSuite::CtExcel, false, 15).await;
}
#[tokio::test]
async fn ike_rekey_auth_lifetime_changes_real_startup_cadence() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 16).await;
}
#[tokio::test]
async fn ike_rekey_authenticated_lifetime_notify_updates_runtime() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 17).await;
}
#[tokio::test]
async fn ike_rekey_unknown_old_delete_retains_owner_until_terminal() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 18).await;
}
#[tokio::test]
async fn ike_rekey_group2_sha1_current_suite_is_preserved() {
    native_network_case_options(NativeSuite::Dito, false, 9).await;
}
#[tokio::test]
async fn ike_rekey_peer_sha512_new_control_keeps_key_lengths_and_roles() {
    native_network_case_options(NativeSuite::Giffgaff, false, 11).await;
}
#[tokio::test]
async fn ike_rekey_peer_then_child_rekey_uses_new_skd_and_role_counter() {
    native_network_case_options(NativeSuite::CtExcel, false, 19).await;
}
#[tokio::test]
async fn ike_rekey_collision_rejects_peer_without_replacing_local_attempt() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 21).await;
}
#[tokio::test]
async fn planned_reauth_jitter_consumed_by_protected_runtime_delete() {
    native_network_case_options(NativeSuite::GcmBaseline, false, 20).await;
}
async fn native_network_case(suite: NativeSuite, application_failure: bool) {
    native_network_case_options(suite, application_failure, 0).await;
}
async fn native_network_case_options(
    suite: NativeSuite,
    application_failure: bool,
    option_case: u8,
) {
    let case_timeout = if matches!(option_case, 12 | 16 | 17) {
        Duration::from_secs(15)
    } else {
        Duration::from_secs(8)
    };
    tokio::time::timeout(case_timeout, async {
        let client_config = suite.client_config();
        let server_identity = client_config.identity.clone();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = tcp.local_addr().unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (stop_tx,stop)=watch::channel(false);
        let server_stop=stop_tx.clone();
        let server = tokio::spawn(async move {
            let (mut control, _) = tcp.accept().await.unwrap();
            let mut greeting = [0; 3];
            control.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            control.write_all(&[5, 0]).await.unwrap();
            let mut request = [0; 10];
            control.read_exact(&mut request).await.unwrap();
            assert_eq!(&request[..3], &[5, 3, 0]);
            let mut reply = vec![5, 0, 0, 1, 127, 0, 0, 1];
            reply.extend(udp.local_addr().unwrap().port().to_be_bytes());
            control.write_all(&reply).await.unwrap();

            let (raw_init, source) = recv(&udp, 500).await;
            let (mut header, body) = IkeHeader::decode(&raw_init).unwrap();
            let initiator_spi = header.initiator_spi;
            let initial = decode_payload_chain(header.next_payload, body).unwrap();
            assert_eq!(
                initial.iter().map(|frame| frame.kind).collect::<Vec<_>>(),
                [33, 34, 40, 41, 41]
            );
            assert_eq!(
                initial[3],
                nat_detection_notify(
                    NatDetectionKind::Source,
                    initiator_spi,
                    0,
                    SocketAddr::from(([0, 0, 0, 0], 0)),
                )
            );
            assert_eq!(
                initial[4],
                nat_detection_notify(
                    NatDetectionKind::Destination,
                    initiator_spi,
                    0,
                    SocketAddr::from(([0_u16; 8], 0)),
                )
            );
            let expected_ike_offer = ike_offer(suite);
            assert_eq!(
                initial.iter().find(|frame| frame.kind == 33).unwrap().body,
                expected_ike_offer
            );
            let ni = &initial.iter().find(|frame| frame.kind == 40).unwrap().body;
            let peer_ke = &initial
                .iter()
                .find(|frame| frame.kind == 34)
                .unwrap()
                .body;
            assert_eq!(u16::from_be_bytes([peer_ke[0], peer_ke[1]]), suite.dh_group());
            let (shared, server_public_key) = if suite.dh_group() == 2 {
                let dh = Modp1024KeyPair::generate().unwrap();
                let shared = dh.shared_secret(&peer_ke[4..]).unwrap();
                (shared, dh.public_key().to_vec())
            } else {
                let dh = Modp2048KeyPair::generate().unwrap();
                let shared = dh.shared_secret(&peer_ke[4..]).unwrap();
                (shared, dh.public_key().to_vec())
            };
            let nr = vec![8; 32];
            let mut ke = suite.dh_group().to_be_bytes().to_vec();
            ke.extend([0, 0]);
            ke.extend(server_public_key);
            header.responder_spi = 22;
            header.flags = IkeHeader::RESPONSE_FLAG;
            let (kind, body) = encode_payload_chain(&[
                frame(33, ike_offer(suite)),
                frame(34, ke),
                frame(40, nr.clone()),
            ])
            .unwrap();
            header.next_payload = kind;
            let init_response = header.encode(&body).unwrap();
            let keys = derive_ike_sa_key_material(
                suite.prf_id(),
                ni,
                &nr,
                &shared,
                header.initiator_spi,
                22,
                suite.ike_encryption_key_len(),
                suite.integrity_key_len(),
            )
            .unwrap();
            send(&udp, source, 500, &init_response).await;

            let golden = "014d004417010000010500001111111111111111111111111111111102050000222222222222222222222222222222220b05000019a5cd42f0c8250e9e34edb24815c777";
            let challenge: Vec<u8> = (0..golden.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&golden[i..i + 2], 16).unwrap())
                .collect();
            let res = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
            let aka = answer_eap_aka_challenge(
                &challenge,
                &server_identity,
                &res,
                &[0x31; 16],
                &[0x42; 16],
            )
            .unwrap();
            let idi = [&[3, 0, 0, 0][..], server_identity.as_slice()].concat();
            let idr = [&[2, 0, 0, 0][..], b"epdg.invalid"].concat();
            let mut local_spi = 0;
            let mut initial_cp=Vec::new();
            for (round, eap) in [vec![1, 9, 0, 5, 1], challenge, vec![3, 77, 0, 4]]
                .into_iter()
                .enumerate()
            {
                let (wire, from) = recv(&udp, 4500).await;
                assert_eq!(from, source);
                assert_eq!(&wire[..4], &[0; 4]);
                let (mut auth_header, frames) =
                    open_ike_sk(suite, &wire[4..], &keys.sk_ei, &keys.sk_ai).unwrap();
                assert_eq!(auth_header.message_id, round as u32 + 1);
                if round == 0 {
                    initial_cp=frames.iter().find(|v|v.kind==47).unwrap().body.clone();
                    assert_eq!(frames[0].body, idi);
                    let child_offer = &frames
                        .iter()
                        .find(|frame| frame.kind == 33)
                        .unwrap()
                        .body;
                    let offered = proletarius_ike::decode_sa_proposals(child_offer).unwrap();
                    local_spi = u32::from_be_bytes(offered[0].spi.as_slice().try_into().unwrap());
                    assert_ne!(local_spi, 0);
                    assert_eq!(*child_offer, esp_offer(suite, local_spi).body);
                }
                if round == 1 {
                    assert_eq!(&frames[0].body[5..], server_identity.as_slice());
                }
                if round == 2 {
                    assert_eq!(frames[0].body, aka.packet);
                }
                auth_header.flags = IkeHeader::RESPONSE_FLAG;
                let reply = seal_ike_sk(
                    suite,
                    auth_header,
                    &keys.sk_er,
                    &keys.sk_ar,
                    &[frame(36, idr.clone()), frame(48, eap)],
                )
                .unwrap();
                send(
                    &udp,
                    source,
                    4500,
                    &proletarius_ike::encode_natt(&reply),
                )
                .await;
            }

            let (wire, _) = recv(&udp, 4500).await;
            let (mut auth_header, frames) =
                open_ike_sk(suite, &wire[4..], &keys.sk_ei, &keys.sk_ai).unwrap();
            assert_eq!(auth_header.message_id, 4);
            if option_case==1 {assert_eq!(frames.len(),2);assert_eq!(frames[1].kind,47);assert_eq!(frames[1].body,initial_cp);}else{assert_eq!(frames.len(),1);}
            let expected = compute_eap_auth(
                suite.prf_id(),
                &aka.msk,
                &raw_init,
                &nr,
                &keys.sk_pi,
                &idi,
            )
            .unwrap();
            assert_eq!(&frames[0].body[4..], &*expected);
            let auth = compute_eap_auth(
                suite.prf_id(),
                &aka.msk,
                &init_response,
                ni,
                &keys.sk_pr,
                &idr,
            )
            .unwrap();
            let mut auth_body = vec![2, 0, 0, 0];
            auth_body.extend_from_slice(&auth);
            auth_header.flags = IkeHeader::RESPONSE_FLAG;
            let mut final_reply_frames=vec![frame(39,auth_body),esp_offer(suite,2),ts(44),ts(45),frame(47,vec![2,0,0,0,0,1,0,4,10,0,0,2])];
            if matches!(option_case,16|22) {final_reply_frames.push(frame(41,[vec![0,0],16403u16.to_be_bytes().to_vec(),(if option_case==22 {0u32}else{6u32}).to_be_bytes().to_vec()].concat()));}
            let reply = seal_ike_sk(
                suite,
                auth_header,
                &keys.sk_er,
                &keys.sk_ar,
                &final_reply_frames,
            )
            .unwrap();
            send(
                &udp,
                source,
                4500,
                &proletarius_ike::encode_natt(&reply),
            )
            .await;

            if option_case==22 {
                let (wire,_)=tokio::time::timeout(Duration::from_secs(1),recv(&udp,4500)).await.unwrap();
                assert_eq!(&wire[..4],&[0;4],"zero lifetime must not start ESP business traffic");
                let (header,frames)=open_ike_sk(suite,&wire[4..],&keys.sk_ei,&keys.sk_ai).unwrap();
                assert_eq!(header.exchange,ExchangeType::Informational);
                assert_eq!(frames.len(),1);assert_eq!(frames[0].kind,42);assert_eq!(frames[0].body,[1,0,0,0]);
                return;
            }
            let mut child = derive_initial_child_sa_keys(
                suite.prf_id(),
                &keys.sk_d,
                ni,
                &nr,
                suite.esp_encryption_key_len(),
                suite.integrity_key_len(),
            )
            .unwrap();
            let mut rx = esp_receiver(
                suite,
                2,
                std::mem::take(&mut child.initiator_encryption),
                std::mem::take(&mut child.initiator_integrity),
            );
            let mut tx = esp_sender(
                suite,
                local_spi,
                std::mem::take(&mut child.responder_encryption),
                std::mem::take(&mut child.responder_integrity),
            );
            if application_failure {
                let mut byte = [0];
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(1), control.read(&mut byte))
                        .await
                        .unwrap()
                        .unwrap(),
                    0
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(80), recv(&udp, 4500))
                        .await
                        .is_err()
                );
                return;
            }

            let mut device = netstack_device::TunnelIpDevice::default();
            let mut interface =
                Interface::new(Config::new(HardwareAddress::Ip), &mut device, Instant::ZERO);
            interface.update_ip_addrs(|addresses| {
                addresses
                    .push("10.0.0.9/24".parse::<IpCidr>().unwrap())
                    .unwrap()
            });
            let mut sockets = SocketSet::new(vec![]);
            let socket = sockets.add(sip_socket());
            let (esp, _) = recv(&udp, 4500).await;
            let ip = rx.open(&esp).unwrap();
            assert_eq!(&ip[12..16], &[10, 0, 0, 2]);
            device.push_inbound(ip).unwrap();
            interface.poll(Instant::ZERO, &mut device, &mut sockets);
            let peer = sockets.get_mut::<udp::Socket>(socket);
            let (data, meta) = peer.recv().unwrap();
            assert_eq!(
                data,
                b"REGISTER sip:ims.example SIP/2.0\r\nContent-Length: 0\r\n\r\n"
            );
            peer.send_slice(
                b"SIP/2.0 200 OK\r\nContent-Length: 0\r\n\r\n",
                meta.endpoint,
            )
            .unwrap();
            interface.poll(Instant::from_millis(1), &mut device, &mut sockets);
            let response = device.pop_outbound().unwrap();
            send(&udp, source, 4500, &tx.seal(&response).unwrap()).await;

            // This second packet proves the client decrypted the responder
            // direction before the server closes the authenticated session.
            let (esp, _) = recv(&udp, 4500).await;
            device.push_inbound(rx.open(&esp).unwrap()).unwrap();
            interface.poll(Instant::from_millis(2), &mut device, &mut sockets);
            assert_eq!(
                sockets.get_mut::<udp::Socket>(socket).recv().unwrap().0,
                b"received"
            );

            if option_case>=9 && option_case!=20 {
                let peer_initiated=matches!(option_case,11|15|19);
                let initial_header=IkeHeader{initiator_spi,responder_spi:22,next_payload:46,version:0x20,exchange:ExchangeType::CreateChildSa,flags:0,message_id:0};
                let started=tokio::time::Instant::now();
                if matches!(option_case,12|17) {
                    // The lost-response case starts its rekey timer only after
                    // both baseline ESP directions have been verified.
                    let notice=seal_ike_sk(suite,IkeHeader{exchange:ExchangeType::Informational,..initial_header.clone()},&keys.sk_er,&keys.sk_ar,&[frame(41,[vec![0,0],16403u16.to_be_bytes().to_vec(),6u32.to_be_bytes().to_vec()].concat())]).unwrap();
                    send(&udp,source,4500,&proletarius_ike::encode_natt(&notice)).await;
                    let (ack,_)=recv(&udp,4500).await;let (h,f)=open_ike_sk(suite,&ack[4..],&keys.sk_ei,&keys.sk_ai).unwrap();
                    assert_eq!(h.exchange,ExchangeType::Informational);
                    assert_eq!((h.initiator_spi,h.responder_spi),(initiator_spi,22));
                    assert_eq!(h.flags,IkeHeader::INITIATOR_FLAG|IkeHeader::RESPONSE_FLAG);
                    assert_eq!(h.message_id,0);assert!(f.is_empty());
                }
                let (new_spi_i,new_spi_r,new_keys)=if peer_initiated {
                    let attempt=crate::ike_rekey::begin(&ike_offer(suite),22).unwrap();
                    let request=seal_ike_sk(suite,initial_header.clone(),&keys.sk_er,&keys.sk_ar,&attempt.frames).unwrap();
                    send(&udp,source,4500,&proletarius_ike::encode_natt(&request)).await;
                    let (wire,_)=recv(&udp,4500).await;let original=wire.clone();let (h,frames)=open_ike_sk(suite,&wire[4..],&keys.sk_ei,&keys.sk_ai).unwrap();assert_eq!(h.flags,IkeHeader::INITIATOR_FLAG|IkeHeader::RESPONSE_FLAG);
                    let material=crate::ike_rekey::finish(&attempt,&frames,&keys.sk_d,initiator_spi,suite.ike_encryption_key_len(),suite.integrity_key_len()).unwrap();
                    send(&udp,source,4500,&proletarius_ike::encode_natt(&request)).await;let (replay,_)=recv(&udp,4500).await;assert_eq!(replay,original);
                    let delete=seal_ike_sk(suite,IkeHeader{exchange:ExchangeType::Informational,message_id:1,..initial_header.clone()},&keys.sk_er,&keys.sk_ar,&[frame(42,vec![1,0,0,0])]).unwrap();
                    send(&udp,source,4500,&proletarius_ike::encode_natt(&delete)).await;let (ack,_)=recv(&udp,4500).await;let (h,f)=open_ike_sk(suite,&ack[4..],&keys.sk_ei,&keys.sk_ai).unwrap();assert_eq!(h.message_id,1);assert!(f.is_empty());
                    if option_case==15 {send(&udp,source,4500,&proletarius_ike::encode_natt(&delete)).await;let (replay,_)=recv(&udp,4500).await;assert_eq!(replay,ack);}
                    (material.spi_i,material.spi_r,material.keys)
                } else {
                    let (wire,_)=recv(&udp,4500).await;
                    let mut rekey_retries = 0;
                    if option_case==21 {
                        let peer_attempt=crate::ike_rekey::begin(&ike_offer(suite),22).unwrap();let collision=seal_ike_sk(suite,initial_header.clone(),&keys.sk_er,&keys.sk_ar,&peer_attempt.frames).unwrap();send(&udp,source,4500,&proletarius_ike::encode_natt(&collision)).await;
                        let (collision_reply,_)=recv_after_rekey_retries(&udp,&wire,None,&mut rekey_retries).await;let (h,f)=open_ike_sk(suite,&collision_reply[4..],&keys.sk_ei,&keys.sk_ai).unwrap();assert_eq!(h.message_id,0);assert_eq!(h.flags,IkeHeader::INITIATOR_FLAG|IkeHeader::RESPONSE_FLAG);assert_eq!(f.len(),1);assert_eq!(f[0].body,[0,0,0,43]);
                        send(&udp,source,4500,&proletarius_ike::encode_natt(&collision)).await;let (replay,_)=recv_after_rekey_retries(&udp,&wire,None,&mut rekey_retries).await;assert_eq!(replay,collision_reply);
                    }
                    if option_case==13 {let (retry,_)=recv(&udp,4500).await;assert_eq!(wire,retry);}
                    if option_case==13 {let mut byte=[0];assert_eq!(tokio::time::timeout(Duration::from_secs(2),control.read(&mut byte)).await.unwrap().unwrap(),0);assert!(tokio::time::timeout(Duration::from_millis(80),recv(&udp,4500)).await.is_err());return;}
                    if matches!(option_case,16|17) {assert!(started.elapsed()>=Duration::from_millis(3500));assert!(started.elapsed()<Duration::from_millis(5500));}
                    let (mut h,frames)=open_ike_sk(suite,&wire[4..],&keys.sk_ei,&keys.sk_ai).unwrap();assert_eq!(h.exchange,ExchangeType::CreateChildSa);assert_eq!(h.message_id,5);
                    // Independent responder key schedule, not the production respond helper.
                    let mut proposal=proletarius_ike::decode_sa_proposals(&frames.iter().find(|v|v.kind==33).unwrap().body).unwrap();let new_i=u64::from_be_bytes(proposal[0].spi.as_slice().try_into().unwrap());assert_ne!(new_i,initiator_spi);proposal[0].spi=77u64.to_be_bytes().to_vec();
                    let ni=&frames.iter().find(|v|v.kind==40).unwrap().body;let nr=vec![0x93;32];let ke=&frames.iter().find(|v|v.kind==34).unwrap().body;
                    let dh=crate::sa_init::ModpKeyPair::generate(suite.dh_group()).unwrap();let shared=dh.shared_secret(&ke[4..]).unwrap();let mut reply_ke=suite.dh_group().to_be_bytes().to_vec();reply_ke.extend([0,0]);reply_ke.extend(dh.public_key());
                    let new_keys=proletarius_ike::derive_rekey_ike_sa_key_material(suite.prf_id(),&keys.sk_d,ni,&nr,&shared,new_i,77,suite.ike_encryption_key_len(),suite.integrity_key_len()).unwrap();
                    h.flags=IkeHeader::RESPONSE_FLAG;let response=seal_ike_sk(suite,h,&keys.sk_er,&keys.sk_ar,&[frame(33,crate::child_rekey::encode(&proposal).unwrap()),frame(40,nr),frame(34,reply_ke)]).unwrap();
                    if option_case==14 {server_stop.send(true).unwrap();}
                    let cached_response=proletarius_ike::encode_natt(&response);
                    if option_case==12 {
                        // Freeze the DH response before withholding it until the
                        // deliberate retry; the retry deadline keeps running.
                        let (retry,_)=recv(&udp,4500).await;
                        assert_eq!(retry,wire,"lost-response retry changed ciphertext or SPI");
                        rekey_retries=1;
                    }
                    if option_case==21 && rekey_retries==0 {
                        // Require one real retry, after the response is fully
                        // prepared, rather than relying on machine load to
                        // exercise collision/retransmission interleaving.
                        let (retry,_)=recv(&udp,4500).await;
                        assert_eq!(retry,wire,"collision replaced the local rekey attempt");
                        rekey_retries=1;
                    }
                    send(&udp,source,4500,&cached_response).await;
                    if option_case==21 {
                        // Earlier retries arrived before a response existed;
                        // answer each with the exact same frozen ciphertext.
                        for _ in 0..rekey_retries {send(&udp,source,4500,&cached_response).await;}
                    }
                    let (wire,_)=recv_after_rekey_retries(&udp,&wire,Some(&cached_response),&mut rekey_retries).await;
                    let (mut h,f)=open_ike_sk(suite,&wire[4..],&keys.sk_ei,&keys.sk_ai).unwrap();
                    assert_eq!(h.exchange,ExchangeType::Informational);
                    assert_eq!((h.initiator_spi,h.responder_spi),(initiator_spi,22));
                    assert_eq!(h.flags,IkeHeader::INITIATOR_FLAG);
                    assert_eq!(h.message_id,6);
                    assert_eq!(f.len(),1);assert_eq!(f[0].kind,42);assert_eq!(f[0].body,[1,0,0,0]);
                    if option_case==18 {let (retry,_)=recv(&udp,4500).await;assert_eq!(retry,wire);let mut byte=[0];assert_eq!(tokio::time::timeout(Duration::from_secs(2),control.read(&mut byte)).await.unwrap().unwrap(),0);assert!(tokio::time::timeout(Duration::from_millis(80),recv(&udp,4500)).await.is_err());return;}
                    h.flags=IkeHeader::RESPONSE_FLAG;let ack=seal_ike_sk(suite,h,&keys.sk_er,&keys.sk_ar,&[]).unwrap();send(&udp,source,4500,&proletarius_ike::encode_natt(&ack)).await;
                    (new_i,77,new_keys)
                };
                if option_case==14 {
                    let (wire,_)=recv(&udp,4500).await;let (h,f)=open_ike_sk(suite,&wire[4..],&new_keys.sk_ei,&new_keys.sk_ai).unwrap();assert_eq!((h.initiator_spi,h.responder_spi,h.message_id),(new_spi_i,new_spi_r,0));assert_eq!(f[0].body,[1,0,0,0]);return;
                }
                let (peer_e,peer_a,local_e,local_a,peer_flag,local_flag)=if peer_initiated {(&new_keys.sk_ei,&new_keys.sk_ai,&new_keys.sk_er,&new_keys.sk_ar,IkeHeader::INITIATOR_FLAG,0)}else{(&new_keys.sk_er,&new_keys.sk_ar,&new_keys.sk_ei,&new_keys.sk_ai,0,IkeHeader::INITIATOR_FLAG)};
                let new_header=IkeHeader{initiator_spi:new_spi_i,responder_spi:new_spi_r,exchange:ExchangeType::Informational,message_id:0,flags:peer_flag,..initial_header};
                let probe=seal_ike_sk(suite,new_header.clone(),peer_e,peer_a,&[]).unwrap();send(&udp,source,4500,&proletarius_ike::encode_natt(&probe)).await;let (wire,_)=recv(&udp,4500).await;let (h,f)=open_ike_sk(suite,&wire[4..],local_e,local_a).unwrap();assert_eq!(h.flags,local_flag|IkeHeader::RESPONSE_FLAG);assert_eq!(h.message_id,0);assert!(f.is_empty());
                // Same CHILD keys and monotonically continued ESP sequence survive IKE replacement.
                sockets.get_mut::<udp::Socket>(socket).send_slice(b"rekey",meta.endpoint).unwrap();interface.poll(Instant::from_millis(3),&mut device,&mut sockets);let packet=device.pop_outbound().unwrap();send(&udp,source,4500,&tx.seal(&packet).unwrap()).await;
                let (wire,_)=recv(&udp,4500).await;device.push_inbound(rx.open(&wire).unwrap()).unwrap();interface.poll(Instant::from_millis(4),&mut device,&mut sockets);assert_eq!(sockets.get_mut::<udp::Socket>(socket).recv().unwrap().0,b"rekey received");
                if option_case==19 {
                    let (wire,_)=recv(&udp,4500).await;let (mut h,f)=open_ike_sk(suite,&wire[4..],local_e,local_a).unwrap();assert_eq!(h.exchange,ExchangeType::CreateChildSa);assert_eq!(h.flags,0);assert_eq!(h.message_id,0);
                    let mut proposal=proletarius_ike::decode_sa_proposals(&f.iter().find(|v|v.kind==33).unwrap().body).unwrap();let new_local=u32::from_be_bytes(proposal[0].spi.as_slice().try_into().unwrap());proposal[0].spi=103u32.to_be_bytes().to_vec();
                    let ni=&f.iter().find(|v|v.kind==40).unwrap().body;let nr=vec![0x94;32];let mut response=vec![frame(33,crate::child_rekey::encode(&proposal).unwrap()),frame(40,nr.clone())];response.extend(f.iter().filter(|v|matches!(v.kind,44|45)).cloned());
                    h.flags=IkeHeader::INITIATOR_FLAG|IkeHeader::RESPONSE_FLAG;let response=seal_ike_sk(suite,h,peer_e,peer_a,&response).unwrap();send(&udp,source,4500,&proletarius_ike::encode_natt(&response)).await;
                    let mut child=proletarius_ike::derive_rekey_child_sa_keys(suite.prf_id(),&new_keys.sk_d,&[],ni,&nr,suite.esp_encryption_key_len(),suite.integrity_key_len()).unwrap();
                    let mut new_rx=esp_receiver(suite,103,std::mem::take(&mut child.initiator_encryption),std::mem::take(&mut child.initiator_integrity));let mut new_tx=esp_sender(suite,new_local,std::mem::take(&mut child.responder_encryption),std::mem::take(&mut child.responder_integrity));
                    let (wire,_)=recv(&udp,4500).await;let (mut h,f)=open_ike_sk(suite,&wire[4..],local_e,local_a).unwrap();assert_eq!(h.flags,0);assert_eq!(h.message_id,1);assert_eq!(f[0].body,[vec![3,4,0,1],local_spi.to_be_bytes().to_vec()].concat());h.flags=IkeHeader::INITIATOR_FLAG|IkeHeader::RESPONSE_FLAG;let ack=seal_ike_sk(suite,h,peer_e,peer_a,&[]).unwrap();send(&udp,source,4500,&proletarius_ike::encode_natt(&ack)).await;
                    sockets.get_mut::<udp::Socket>(socket).send_slice(b"rekey",meta.endpoint).unwrap();interface.poll(Instant::from_millis(5),&mut device,&mut sockets);let packet=device.pop_outbound().unwrap();send(&udp,source,4500,&new_tx.seal(&packet).unwrap()).await;let (wire,_)=recv(&udp,4500).await;device.push_inbound(new_rx.open(&wire).unwrap()).unwrap();interface.poll(Instant::from_millis(6),&mut device,&mut sockets);assert_eq!(sockets.get_mut::<udp::Socket>(socket).recv().unwrap().0,b"rekey received");
                }
                if matches!(option_case,16|17) {
                    // An IKE rekey derives fresh keys but performs no EAP. The
                    // authenticated original deadline must survive installation.
                    let (wire,_)=recv(&udp,4500).await;
                    let (h,f)=open_ike_sk(suite,&wire[4..],local_e,local_a).unwrap();
                    assert_eq!(h.exchange,ExchangeType::Informational);
                    assert_eq!(f.len(),1);assert_eq!(f[0].kind,42);assert_eq!(f[0].body,[1,0,0,0]);
                    assert!(started.elapsed()<Duration::from_millis(6500),"rekey extended AUTH_LIFETIME");
                    return;
                }
                let delete=seal_ike_sk(suite,IkeHeader{message_id:1,..new_header},peer_e,peer_a,&[frame(42,vec![1,0,0,0])]).unwrap();send(&udp,source,4500,&proletarius_ike::encode_natt(&delete)).await;let (wire,_)=recv(&udp,4500).await;let (h,f)=open_ike_sk(suite,&wire[4..],local_e,local_a).unwrap();assert_eq!(h.message_id,1);assert!(f.is_empty());return;
            }
            if matches!(option_case,4|5|7|8) {
                let (wire,_)=recv(&udp,4500).await;
                if matches!(option_case,5|7|8) {
                    // Make a retransmission deterministic, including PFS: the
                    // responder has not acknowledged this immutable request.
                    let (retry,_)=recv(&udp,4500).await;
                    assert_eq!(wire,retry,"CHILD retry changed ciphertext or SPI");
                }
                if option_case==8 {let mut byte=[0];assert_eq!(tokio::time::timeout(Duration::from_secs(1),control.read(&mut byte)).await.unwrap().unwrap(),0);assert!(tokio::time::timeout(Duration::from_millis(80),recv(&udp,4500)).await.is_err());return;}
let (mut rekey_header,rekey_frames)=open_ike_sk(suite,&wire[4..],&keys.sk_ei,&keys.sk_ai).unwrap();
                assert_eq!(rekey_header.exchange,ExchangeType::CreateChildSa);assert_eq!(rekey_header.message_id,5);
                let offer=&rekey_frames.iter().find(|v|v.kind==33).unwrap().body;
                let mut proposal=proletarius_ike::decode_sa_proposals(offer).unwrap();proposal.truncate(1);let new_local=u32::from_be_bytes(proposal[0].spi.as_slice().try_into().unwrap());proposal[0].spi=103u32.to_be_bytes().to_vec();
                let ni=&rekey_frames.iter().find(|v|v.kind==40).unwrap().body;let nr=vec![0x72;32];
                let mut response=vec![frame(33,crate::child_rekey::encode(&proposal).unwrap()),frame(40,nr.clone())];
                let shared=if option_case==5 {let peer=Modp2048KeyPair::generate().unwrap();let ke=&rekey_frames.iter().find(|v|v.kind==34).unwrap().body;let mut reply_ke=14u16.to_be_bytes().to_vec();reply_ke.extend([0,0]);reply_ke.extend(peer.public_key());response.push(frame(34,reply_ke));peer.shared_secret(&ke[4..]).unwrap()}else{Zeroizing::new(Vec::new())};
                response.extend(rekey_frames.iter().filter(|v|matches!(v.kind,44|45)).cloned());
                rekey_header.flags=IkeHeader::RESPONSE_FLAG;let reply=seal_ike_sk(suite,rekey_header,&keys.sk_er,&keys.sk_ar,&response).unwrap();send(&udp,source,4500,&proletarius_ike::encode_natt(&reply)).await;
                let mut child=proletarius_ike::derive_rekey_child_sa_keys(suite.prf_id(),&keys.sk_d,&shared,ni,&nr,suite.esp_encryption_key_len(),suite.integrity_key_len()).unwrap();
                let mut new_rx=esp_receiver(suite,103,std::mem::take(&mut child.initiator_encryption),std::mem::take(&mut child.initiator_integrity));
                let mut new_tx=esp_sender(suite,new_local,std::mem::take(&mut child.responder_encryption),std::mem::take(&mut child.responder_integrity));
                // PFS work or scheduling may leave an exact retry queued even
                // after our response. Replay only the frozen response for that
                // exact ciphertext; a new request/SPI must fail this fixture.
                let delete_wire=loop {
                    let (next,_)=recv(&udp,4500).await;
                    if next==wire {
                        send(&udp,source,4500,&proletarius_ike::encode_natt(&reply)).await;
                        continue;
                    }
                    break next;
                };
                let (mut delete_header,delete_frames)=open_ike_sk(suite,&delete_wire[4..],&keys.sk_ei,&keys.sk_ai).unwrap();assert_eq!(delete_header.exchange,ExchangeType::Informational);assert_eq!(delete_header.message_id,6);assert_eq!(delete_frames[0].body,[vec![3,4,0,1],local_spi.to_be_bytes().to_vec()].concat());
                delete_header.flags=IkeHeader::RESPONSE_FLAG;let ack=seal_ike_sk(suite,delete_header,&keys.sk_er,&keys.sk_ar,&[]).unwrap();send(&udp,source,4500,&proletarius_ike::encode_natt(&ack)).await;
                sockets.get_mut::<udp::Socket>(socket).send_slice(b"rekey",meta.endpoint).unwrap();interface.poll(Instant::from_millis(3),&mut device,&mut sockets);let packet=device.pop_outbound().unwrap();send(&udp,source,4500,&new_tx.seal(&packet).unwrap()).await;
                let (wire,_)=recv(&udp,4500).await;device.push_inbound(new_rx.open(&wire).unwrap()).unwrap();interface.poll(Instant::from_millis(4),&mut device,&mut sockets);assert_eq!(sockets.get_mut::<udp::Socket>(socket).recv().unwrap().0,b"rekey received");
            }
            if option_case==6 {
                let old=crate::InitialChildSaSelection{local_spi:2,remote_spi:local_spi,encryption_transform:12,integrity_transform:Some(12),encryption_key_len:suite.esp_encryption_key_len(),integrity_key_len:suite.integrity_key_len()};
                let selectors=vec![ts(44),ts(45)];let peer_attempt=crate::child_rekey::begin(&esp_offer(suite,2).body,&selectors,2,14).unwrap();
                let header=IkeHeader{initiator_spi,responder_spi:22,next_payload:46,version:0x20,exchange:ExchangeType::CreateChildSa,flags:0,message_id:0};
                let request=seal_ike_sk(suite,header.clone(),&keys.sk_er,&keys.sk_ar,&peer_attempt.frames).unwrap();send(&udp,source,4500,&proletarius_ike::encode_natt(&request)).await;
                let (wire,_)=recv(&udp,4500).await;let original_response=wire.clone();let (response_header,response)=open_ike_sk(suite,&wire[4..],&keys.sk_ei,&keys.sk_ai).unwrap();assert_eq!(response_header.exchange,ExchangeType::CreateChildSa);assert_eq!(response_header.flags,IkeHeader::INITIATOR_FLAG|IkeHeader::RESPONSE_FLAG);
                let mut material=crate::child_rekey::finish(&peer_attempt,&response,suite.prf_id(),&keys.sk_d,&old).unwrap();
                let mut new_tx=esp_sender(suite,material.selection.remote_spi,std::mem::take(&mut material.keys.initiator_encryption),std::mem::take(&mut material.keys.initiator_integrity));
                let mut new_rx=esp_receiver(suite,material.selection.local_spi,std::mem::take(&mut material.keys.responder_encryption),std::mem::take(&mut material.keys.responder_integrity));
                sockets.get_mut::<udp::Socket>(socket).send_slice(b"rekey",meta.endpoint).unwrap();interface.poll(Instant::from_millis(3),&mut device,&mut sockets);let packet=device.pop_outbound().unwrap();send(&udp,source,4500,&new_tx.seal(&packet).unwrap()).await;
                let (wire,_)=recv(&udp,4500).await;device.push_inbound(new_rx.open(&wire).unwrap()).unwrap();interface.poll(Instant::from_millis(4),&mut device,&mut sockets);assert_eq!(sockets.get_mut::<udp::Socket>(socket).recv().unwrap().0,b"rekey received");
                // Retransmission of the exact request must only replay the frozen response.
                send(&udp,source,4500,&proletarius_ike::encode_natt(&request)).await;let (replay,_)=recv(&udp,4500).await;assert_eq!(replay,original_response);
                let mut body=vec![3,4,0,1];body.extend_from_slice(&2u32.to_be_bytes());let delete=seal_ike_sk(suite,IkeHeader{exchange:ExchangeType::Informational,message_id:1,..header},&keys.sk_er,&keys.sk_ar,&[frame(42,body)]).unwrap();send(&udp,source,4500,&proletarius_ike::encode_natt(&delete)).await;
                let (wire,_)=recv(&udp,4500).await;let (_,ack)=open_ike_sk(suite,&wire[4..],&keys.sk_ei,&keys.sk_ai).unwrap();assert_eq!(ack[0].body,[vec![3,4,0,1],local_spi.to_be_bytes().to_vec()].concat());
            }
            if matches!(option_case,3|20) {
                let (wire,_)=recv(&udp,4500).await;
                let (header,frames)=open_ike_sk(suite,&wire[4..],&keys.sk_ei,&keys.sk_ai).unwrap();
                assert_eq!(header.exchange,ExchangeType::Informational);assert_eq!(frames.len(),1);assert_eq!(frames[0].kind,42);assert_eq!(frames[0].body,[1,0,0,0]);
                return;
            }
            if option_case==2 || option_case>=4 {
                let mut packet=[0;2048];assert!(tokio::time::timeout(Duration::from_millis(110),udp.recv_from(&mut packet)).await.is_err(),"disabled DPD sent a probe");
            } else {
            // Exercise the runtime DPD path, including authentication failure
            // isolation, before the peer-initiated DELETE closes the SA.
            let (wire, _) = recv(&udp, 4500).await;
            assert_eq!(&wire[..4], &[0; 4]);
            let (mut dpd_header, frames) =
                open_ike_sk(suite, &wire[4..], &keys.sk_ei, &keys.sk_ai).unwrap();
            assert_eq!(dpd_header.exchange, ExchangeType::Informational);
            assert_eq!(dpd_header.flags, IkeHeader::INITIATOR_FLAG);
            assert_eq!(dpd_header.message_id, 5);
            assert!(frames.is_empty());
            dpd_header.flags = IkeHeader::RESPONSE_FLAG;
            let dpd_response =
                seal_ike_sk(suite, dpd_header, &keys.sk_er, &keys.sk_ar, &[]).unwrap();
            let mut corrupt_dpd = dpd_response.clone();
            *corrupt_dpd.last_mut().unwrap() ^= 1;
            send(
                &udp,
                source,
                4500,
                &proletarius_ike::encode_natt(&corrupt_dpd),
            )
            .await;
            send(
                &udp,
                source,
                4500,
                &proletarius_ike::encode_natt(&dpd_response),
            )
            .await;

            }
            let delete = seal_ike_sk(
                suite,
                IkeHeader {
                    initiator_spi,
                    responder_spi: 22,
                    next_payload: 46,
                    version: 0x20,
                    exchange: ExchangeType::Informational,
                    flags: 0,
                    message_id: if option_case==6 {2}else{0},
                },
                &keys.sk_er,
                &keys.sk_ar,
                &[frame(42, vec![1, 0, 0, 0])],
            )
            .unwrap();
            let mut corrupt_delete = delete.clone();
            *corrupt_delete.last_mut().unwrap() ^= 1;
            send(
                &udp,
                source,
                4500,
                &proletarius_ike::encode_natt(&corrupt_delete),
            )
            .await;
            send(
                &udp,
                source,
                4500,
                &proletarius_ike::encode_natt(&delete),
            )
            .await;
            let (ack, _) = recv(&udp, 4500).await;
            let (ack_header, frames) =
                open_ike_sk(suite, &ack[4..], &keys.sk_ei, &keys.sk_ai).unwrap();
            assert_eq!(ack_header.exchange, ExchangeType::Informational);
            assert_eq!(
                ack_header.flags,
                IkeHeader::INITIATOR_FLAG | IkeHeader::RESPONSE_FLAG
            );
            assert_eq!(ack_header.message_id, if option_case==6 {2}else{0});
            assert!(frames.is_empty());
        });

        let transport = EpdgDatagramTransport::open(
            ProxyMode::Socks5UdpDomain,
            "epdg.invalid",
            500,
            Some(proxy),
        )
        .await
        .unwrap();

        let mut tunnel = crate::start_configured_native_tunnel_with_options(
            transport,
            NativeTunnelSettings {
                identity: &client_config.identity,
                apn: &client_config.apn,
                device_identity_imei: None,
                ike_proposals: &client_config.proposals.ike,
                esp_proposals: &client_config.proposals.esp,
                ip_stack: client_config.ip_stack,
                checkcode_mode: client_config.checkcode_mode,
                initial_contact: false,
                retain_eap_direct_material: false,
            },
            crate::NativeTunnelOptions {cp_in_final_auth:option_case==1,disable_dpd:option_case>=2,reauth_interval:matches!(option_case,3|20).then(||if option_case==20 {Duration::from_millis(500)}else{Duration::from_millis(160)}),reauth_jitter:option_case==20,child_rekey_interval:if option_case==19 {Some(Duration::from_millis(600))}else{matches!(option_case,4|5|7|8).then(||Duration::from_millis(200))},ike_rekey_interval:(option_case>=9&&option_case!=20).then(||if option_case==12 {Duration::from_secs(20)}else if matches!(option_case,11|15|16|17|19){Duration::from_secs(10)}else{Duration::from_millis(200)}),rekey_pfs:if option_case==5||option_case==6 {14}else{0},..Default::default()},
            |challenge| async move {
                assert_eq!(challenge.rand, [0x11; 16]);
                Ok::<_, ()>(SimAkaResult::Success {
                    res: vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
                    ck: [0x31; 16],
                    ik: [0x42; 16],
                })
            },
            stop.clone(),
        )
        .await
        .unwrap();
        assert!(tunnel.take_eap_direct_auth_material().is_none());
        assert_eq!(
            tunnel.configuration().ipv4_addresses[0].to_string(),
            "10.0.0.2"
        );
        let mut sockets = SocketSet::new(vec![]);
        let socket = sockets.add(sip_socket());
        let (_wake, wake) = watch::channel(0);
        let mut sent = false;
        let mut received = false;
        let mut rekey_received=false;
        let result = tunnel
            .run_network(
                sockets,
                stop,
                wake,
                if option_case>=9 {Duration::from_secs(10)}else{Duration::from_millis(150)},
                if option_case==5 {Duration::from_millis(300)}else if option_case>=9 {Duration::from_millis(300)}else{Duration::from_millis(80)},
                if option_case==5 {3}else{2},
                |_, sockets| {
                    if application_failure {
                        return Err(());
                    }
                    let socket = sockets.get_mut::<udp::Socket>(socket);
                    if !sent {
                        socket
                            .send_slice(
                                b"REGISTER sip:ims.example SIP/2.0\r\nContent-Length: 0\r\n\r\n",
                                (
                                    "10.0.0.9"
                                        .parse::<smoltcp::wire::IpAddress>()
                                        .unwrap(),
                                    5060,
                                ),
                            )
                            .unwrap();
                        sent = true;
                    }
                    if socket.can_recv() {
                        let (data, meta) = socket.recv().unwrap();
                        if data==b"rekey" {rekey_received=true;socket.send_slice(b"rekey received",meta.endpoint).unwrap();}
                        else {assert_eq!(data,b"SIP/2.0 200 OK\r\nContent-Length: 0\r\n\r\n");received=true;socket.send_slice(b"received",meta.endpoint).unwrap();}
                    }
                    Ok::<_, ()>(())
                },
            )
            .await;
        if application_failure {
            assert!(matches!(
                result,
                Err(NativeNetworkError::Network(
                    netstack_runtime::NetworkRunError::Application(())
                ))
            ));
            assert!(!sent && !received);
        } else if option_case==22 {assert!(matches!(result,Err(NativeNetworkError::Tunnel(IkeAuthError::PlannedReauthentication))));assert!(!received);
        } else if matches!(option_case,8|13|18) {assert!(matches!(result,Err(NativeNetworkError::Tunnel(IkeAuthError::Timeout))));assert!(sent&&received);
        } else if option_case==14 {assert!(result.is_ok(),"{result:?}");assert!(sent&&received);
        } else if matches!(option_case,3|16|17|20) {
            assert!(matches!(result,Err(NativeNetworkError::Tunnel(IkeAuthError::PlannedReauthentication))));assert!(sent&&received);
        } else {
            assert!(matches!(
                result,
                Err(NativeNetworkError::Tunnel(IkeAuthError::DataPlane(
                    ChildDataError::PeerDeleted
                )))
            ),"{result:?}");
            assert!(sent && received);
        }
        if option_case>=4 && !matches!(option_case,8|13|14|18|20|22) {assert!(rekey_received);}
        server.await.unwrap();
    })
    .await
    .unwrap();
}
