//! Offline composition check for the SIM World negotiated ESP suite. All
//! identities, addresses and keys below are synthetic; UDP stays on loopback.
use super::*;
use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::NoPadding};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::net::{IpAddr, Ipv4Addr};
use tokio::{net::UdpSocket, sync::mpsc, sync::watch};
use zeroize::Zeroizing;

use crate::{
    EapHandshakeResult, ProxyMode, ims_network_runtime::ImsNetworkRuntime,
    netstack_runtime::TunnelNetwork, sa_init::SaInitResponse,
};

const LOCAL_SPI: u32 = 0x1122_3344;
const REMOTE_SPI: u32 = 0x5566_7788;
const LOCAL_IP: [u8; 4] = [10, 0, 0, 2];
const PCSCF_IP: [u8; 4] = [10, 20, 30, 9];

// Independent Python 3 standard-library oracle, not Rust derive_* or ESP code:
// import hashlib, hmac
// key = bytes([1])*32; seed = bytes([6])*32 + bytes([7])*32
// t = b''; material = b''
// for counter in range(1, 5):
//     t = hmac.digest(key, t + seed + bytes([counter]), hashlib.sha256)
//     material += t
// Split material into 32-byte out_enc, out_auth, in_enc, in_auth in that order.
const OUT_ENC: &str = "05197dbbc6b60328cfe44ad9187e9227b000781b4f0ec74aa3ddcb76461683ae";
const OUT_AUTH: &str = "1efce4a1238ff931fcc185c5a709ca467f4b0942805445127a339f649125b0c9";
const IN_ENC: &str = "f6fc48f6652f5b6351a78a74f79961ed071e41003e329259134951c8ee90efd4";
const IN_AUTH: &str = "59a9646ac5bd769bdec706985e540f0daeda1a177b204954b832b888b8c0e919";

fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn proposal(spi: u32) -> PayloadFrame {
    let mut body = vec![0, 0, 0, 40, 1, 3, 4, 3];
    body.extend_from_slice(&spi.to_be_bytes());
    body.extend_from_slice(&[
        3, 0, 0, 12, 1, 0, 0, 12, 0x80, 14, 1, 0, // AES-CBC, 256 bits
        3, 0, 0, 8, 3, 0, 0, 12, // HMAC-SHA256-128
        0, 0, 0, 8, 5, 0, 0, 0, // No ESN
    ]);
    PayloadFrame {
        kind: 33,
        critical: false,
        body,
    }
}

fn selectors(kind: u8) -> PayloadFrame {
    let mut body = vec![1, 0, 0, 0, 7, 0, 0, 16, 0, 0, 255, 255];
    if kind == 44 {
        body.extend_from_slice(&LOCAL_IP);
        body.extend_from_slice(&LOCAL_IP);
    } else {
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&[255; 4]);
    }
    PayloadFrame {
        kind,
        critical: false,
        body,
    }
}

// This deliberately never calls EspReceiver, ESP framing helpers, PRF+, or
// smoltcp's packet/checksum parsers. Only AES and HMAC primitives are shared.
fn independent_esp_plaintext(wire: &[u8]) -> Result<Vec<u8>, &'static str> {
    if wire.len() < 56 || (wire.len() - 40) % 16 != 0 {
        return Err("CBC ESP length");
    }
    if wire[..4] != REMOTE_SPI.to_be_bytes() || wire[4..8] != 1_u32.to_be_bytes() {
        return Err("outbound SPI or first sequence");
    }
    let authenticated_len = wire.len() - 16;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&hex(OUT_AUTH)).unwrap();
    mac.update(&wire[..authenticated_len]);
    mac.verify_truncated_left(&wire[authenticated_len..])
        .map_err(|_| "HMAC-SHA256-128")?;
    let mut plain = wire[24..authenticated_len].to_vec();
    cbc::Decryptor::<aes::Aes256>::new_from_slices(&hex(OUT_ENC), &wire[8..24])
        .unwrap()
        .decrypt_padded::<NoPadding>(&mut plain)
        .map_err(|_| "CBC ciphertext")?;
    let pad_len = usize::from(plain[plain.len() - 2]);
    if plain[plain.len() - 1] != 4 || pad_len + 2 > plain.len() {
        return Err("IPv4 ESP trailer");
    }
    let inner_len = plain.len() - pad_len - 2;
    if plain[inner_len..plain.len() - 2]
        .iter()
        .enumerate()
        .any(|(index, value)| usize::from(*value) != index + 1)
    {
        return Err("numbered ESP padding");
    }
    plain.truncate(inner_len);
    Ok(plain)
}

fn checksum_valid(bytes: &[u8]) -> bool {
    let mut sum: u32 = bytes
        .chunks(2)
        .map(|pair| u32::from(u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)])))
        .sum();
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    sum == 0xffff
}

fn assert_inner_syn(packet: &[u8]) {
    assert!(packet.len() >= 40);
    assert_eq!(packet[0] >> 4, 4);
    let ip_header_len = usize::from(packet[0] & 15) * 4;
    assert!(ip_header_len >= 20 && ip_header_len + 20 <= packet.len());
    assert_eq!(
        usize::from(u16::from_be_bytes([packet[2], packet[3]])),
        packet.len()
    );
    assert_eq!(packet[9], 6);
    assert_eq!(&packet[12..16], &LOCAL_IP);
    assert_eq!(&packet[16..20], &PCSCF_IP);
    assert!(checksum_valid(&packet[..ip_header_len]));
    let tcp = &packet[ip_header_len..];
    assert_eq!(u16::from_be_bytes([tcp[0], tcp[1]]), 5060);
    assert_eq!(u16::from_be_bytes([tcp[2], tcp[3]]), 5060);
    assert_eq!(tcp[13], 2, "first TCP packet is SYN without ACK or payload");
    assert_eq!(usize::from(tcp[12] >> 4) * 4, tcp.len());
    let mut pseudo_and_segment = packet[12..20].to_vec();
    pseudo_and_segment.extend_from_slice(&[0, 6]);
    pseudo_and_segment.extend_from_slice(&(tcp.len() as u16).to_be_bytes());
    pseudo_and_segment.extend_from_slice(tcp);
    assert!(checksum_valid(&pseudo_and_segment));
}

#[tokio::test]
async fn final_eap_cbc_child_emits_independently_verified_first_tcp_syn() {
    timeout(Duration::from_secs(3), exercise()).await.unwrap();
}

async fn exercise() {
    let peer = std::sync::Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
    let header = IkeHeader {
        initiator_spi: 0x1122_3344_5566_7788,
        responder_spi: 0x8877_6655_4433_2211,
        next_payload: 0,
        version: 0x20,
        exchange: ExchangeType::IkeSaInit,
        flags: IkeHeader::RESPONSE_FLAG,
        message_id: 0,
    };
    let sa_response = header.encode(&[]).unwrap();
    let mut session = IkeAuthSession::from_sa_init(SaInitKeySchedule {
        initiator_nonce: vec![6; 32],
        prf_id: 5,
        initiator_spi: header.initiator_spi,
        encryption_transform: 12,
        integrity_transform: Some(12),
        response: SaInitResponse {
            request_packet: vec![3; 28],
            raw_packet: sa_response.clone(),
            responder_spi: header.responder_spi,
            selected_sa: vec![],
            selected_proposal: None,
            dh_group: 14,
            peer_public_key: vec![],
            responder_nonce: vec![7; 32],
        },
        keys: IkeSaKeyMaterial {
            sk_d: vec![1; 32],
            sk_ai: vec![0x71; 32],
            sk_ar: vec![0x81; 32],
            sk_ei: vec![0x51; 32],
            sk_er: vec![0x61; 32],
            sk_pi: vec![2; 32],
            sk_pr: vec![3; 32],
        },
    })
    .unwrap();
    // A synthetic pinned NAT-T port avoids reserving host UDP 4500. It keeps
    // the real transparent send_esp path while sending only to localhost.
    let transport = EpdgDatagramTransport::open(
        ProxyMode::Direct(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        "unused.invalid",
        peer.local_addr().unwrap().port(),
        None,
    )
    .await
    .unwrap()
    .enable_pinned_natt();
    let remote = peer.clone();
    let responder = tokio::spawn(async move {
        let mut buffer = [0; 2048];
        let (n, source) = remote.recv_from(&mut buffer).await.unwrap();
        let (mut request, offered) =
            open_ike_sk_cbc(&buffer[..n], 12, 12, &[0x51; 32], &[0x71; 32]).unwrap();
        assert_eq!(request.message_id, 1);
        assert_eq!(offered[1], proposal(LOCAL_SPI));
        request.flags = IkeHeader::RESPONSE_FLAG;
        let identity = b"\x02\0\0\0epdg.example";
        let response = seal_ike_sk_cbc(
            request,
            12,
            12,
            &[0x61; 32],
            &[0x81; 32],
            &[
                PayloadFrame {
                    kind: 36,
                    critical: false,
                    body: identity.to_vec(),
                },
                PayloadFrame {
                    kind: 48,
                    critical: false,
                    body: vec![3, 7, 0, 4],
                },
            ],
        )
        .unwrap();
        remote.send_to(&response, source).await.unwrap();
        let (n, final_source) = remote.recv_from(&mut buffer).await.unwrap();
        assert_eq!(final_source, source, "CHILD uses the existing UDP mapping");
        let (mut request, frames) =
            open_ike_sk_cbc(&buffer[..n], 12, 12, &[0x51; 32], &[0x71; 32]).unwrap();
        assert_eq!(request.message_id, 2);
        assert_eq!(frames[0].kind, 39);
        assert_eq!(
            &frames[0].body[4..],
            proletarius_ike::compute_eap_auth(
                5,
                &[1; 64],
                &[3; 28],
                &[7; 32],
                &[2; 32],
                b"AKA-RESP",
            )
            .unwrap()
            .as_slice()
        );
        let mut auth = vec![2, 0, 0, 0];
        auth.extend_from_slice(
            &proletarius_ike::compute_eap_auth(
                5,
                &[1; 64],
                &sa_response,
                &[6; 32],
                &[3; 32],
                identity,
            )
            .unwrap(),
        );
        request.flags = IkeHeader::RESPONSE_FLAG;
        let mut config = vec![2, 0, 0, 0, 0, 1, 0, 4];
        config.extend_from_slice(&LOCAL_IP);
        config.extend_from_slice(&[0, 20, 0, 4]);
        config.extend_from_slice(&PCSCF_IP);
        let response = seal_ike_sk_cbc(
            request,
            12,
            12,
            &[0x61; 32],
            &[0x81; 32],
            &[
                PayloadFrame {
                    kind: 39,
                    critical: false,
                    body: auth,
                },
                proposal(REMOTE_SPI),
                selectors(44),
                selectors(45),
                PayloadFrame {
                    kind: 47,
                    critical: false,
                    body: config,
                },
            ],
        )
        .unwrap();
        remote.send_to(&response, source).await.unwrap();
        source
    });
    let response = session
        .exchange(
            &transport,
            &[
                PayloadFrame {
                    kind: 35,
                    critical: false,
                    body: b"AKA-RESP".to_vec(),
                },
                proposal(LOCAL_SPI),
                selectors(44),
                selectors(45),
                PayloadFrame {
                    kind: 47,
                    critical: false,
                    body: vec![1, 0, 0, 0, 0, 1, 0, 0],
                },
            ],
            1,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
    let (session, child) = session
        .finish_eap_authentication(
            &transport,
            EapHandshakeResult {
                msk: Zeroizing::new(vec![1; 64]),
                k_aut: Zeroizing::new(vec![1; 16]),
                last_response: response,
                eap_direct_material: None,
            },
        )
        .await
        .unwrap();
    let source = responder.await.unwrap();
    assert_eq!(session.phase(), SessionPhase::Established);
    assert_eq!(
        (child.selection.local_spi, child.selection.remote_spi),
        (LOCAL_SPI, REMOTE_SPI)
    );
    assert_eq!(
        (
            child.selection.encryption_transform,
            child.selection.integrity_transform
        ),
        (12, Some(12))
    );
    assert_eq!(child.keys.initiator_encryption, hex(OUT_ENC));
    assert_eq!(child.keys.initiator_integrity, hex(OUT_AUTH));
    assert_eq!(child.keys.responder_encryption, hex(IN_ENC));
    assert_eq!(child.keys.responder_integrity, hex(IN_AUTH));
    let mut plane = child.into_data_plane().unwrap();
    assert_eq!(
        plane.configuration().pcscf_servers,
        vec![IpAddr::from(PCSCF_IP)]
    );
    let network = TunnelNetwork::new(plane.configuration()).unwrap();
    let (wake_tx, wake) = watch::channel(0);
    let (tcp, _security, runtime) = ImsNetworkRuntime::new(wake_tx);
    let (_in_tx, in_rx) = mpsc::channel(4);
    let (out_tx, mut out_rx) = mpsc::channel(4);
    let (stop_tx, stop) = watch::channel(false);
    let runner = tokio::spawn(network.run_managed(
        smoltcp::iface::SocketSet::new(vec![]),
        in_rx,
        out_tx,
        stop,
        wake,
        runtime,
    ));
    let pending = tcp
        .dial((Ipv4Addr::from(PCSCF_IP), 5060).into(), 5060)
        .await
        .unwrap();
    let syn = out_rx.recv().await.unwrap();
    plane.send_ip(&transport, &syn).await.unwrap();
    let mut wire = [0; 2048];
    let (n, esp_source) = peer.recv_from(&mut wire).await.unwrap();
    assert_eq!(
        esp_source, source,
        "ESP retains the authenticated UDP channel"
    );
    let wire = &wire[..n];
    let inner = independent_esp_plaintext(wire).unwrap();
    assert_eq!(inner, *syn);
    assert_inner_syn(&inner);
    for offset in [0, 4, 8, 24, wire.len() - 1] {
        let mut damaged = wire.to_vec();
        damaged[offset] ^= 1;
        assert!(independent_esp_plaintext(&damaged).is_err());
    }
    assert_eq!(plane.traffic_probe().snapshot().sent_esp, 1);
    stop_tx.send(true).unwrap();
    runner.await.unwrap().unwrap();
    drop(pending);
}
