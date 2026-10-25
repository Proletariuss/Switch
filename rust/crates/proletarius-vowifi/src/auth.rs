use std::fmt;
use std::time::Duration;

use proletarius_ike::sk::{
    SkPayloadError, open_ike_sk_cbc, open_ike_sk_gcm, seal_ike_sk_cbc, seal_ike_sk_gcm,
};
use proletarius_ike::{
    ExchangeType, IkeHeader, IkeProtocolError, IkeSaKeyMaterial, PayloadFrame, ResponseDisposition,
    SessionPhase, SessionTracker,
};
use subtle::ConstantTimeEq;
use tokio::time::timeout;

use crate::{
    DatagramTransportError, EpdgDatagramTransport, EpdgPacket, sa_init::SaInitKeySchedule,
};

#[cfg(test)]
#[path = "auth/cbc_child_tests.rs"]
mod cbc_child_tests;

#[cfg(test)]
mod tests {
    use proletarius_ike::{ExchangeType, IkeHeader};
    use std::{
        net::{IpAddr, Ipv4Addr},
        time::Duration,
    };

    use proletarius_ike::sk::{open_ike_sk_cbc, open_ike_sk_gcm, seal_ike_sk_cbc, seal_ike_sk_gcm};
    use proletarius_ike::{IkeSaKeyMaterial, PayloadFrame};
    use tokio::net::UdpSocket;

    use super::IkeAuthSession;
    use crate::{
        EpdgDatagramTransport, ProxyMode,
        sa_init::{SaInitKeySchedule, SaInitResponse},
    };

    #[test]
    fn cbc_session_dispatches_both_carrier_suites_and_directions() {
        for (integrity, encryption_len, integrity_len) in [(12, 16, 32), (14, 32, 64)] {
            let initiator_spi = 0x1122_3344_5566_7788;
            let responder_spi = 0x8877_6655_4433_2211;
            let response_header = IkeHeader {
                initiator_spi,
                responder_spi,
                next_payload: 0,
                version: 0x20,
                exchange: ExchangeType::IkeSaInit,
                flags: IkeHeader::RESPONSE_FLAG,
                message_id: 0,
            };
            let initiator_encryption = vec![0x51; encryption_len];
            let responder_encryption = vec![0x61; encryption_len];
            let initiator_integrity = vec![0x71; integrity_len];
            let responder_integrity = vec![0x81; integrity_len];
            let session = IkeAuthSession::from_sa_init(SaInitKeySchedule {
                initiator_nonce: vec![6; 32],
                prf_id: if integrity == 12 { 5 } else { 7 },
                initiator_spi,
                encryption_transform: 12,
                integrity_transform: Some(integrity),
                response: SaInitResponse {
                    request_packet: vec![3; 28],
                    raw_packet: response_header.encode(&[]).unwrap(),
                    responder_spi,
                    selected_sa: vec![],
                    selected_proposal: None,
                    dh_group: 14,
                    peer_public_key: vec![],
                    responder_nonce: vec![7; 32],
                },
                keys: IkeSaKeyMaterial {
                    sk_d: vec![1; integrity_len],
                    sk_ai: initiator_integrity.clone(),
                    sk_ar: responder_integrity.clone(),
                    sk_ei: initiator_encryption.clone(),
                    sk_er: responder_encryption.clone(),
                    sk_pi: vec![2; integrity_len],
                    sk_pr: vec![3; integrity_len],
                },
            })
            .unwrap();
            let frames = [PayloadFrame {
                kind: 48,
                critical: false,
                body: vec![2, 7, 0, 5, 1],
            }];
            let request_header = IkeHeader {
                initiator_spi,
                responder_spi,
                next_payload: 0,
                version: 0x20,
                exchange: ExchangeType::IkeAuth,
                flags: IkeHeader::INITIATOR_FLAG,
                message_id: 1,
            };
            let request = session.seal_sk(request_header.clone(), &frames).unwrap();
            assert_eq!(
                open_ike_sk_cbc(
                    &request,
                    12,
                    integrity,
                    &initiator_encryption,
                    &initiator_integrity,
                )
                .unwrap()
                .1,
                frames
            );
            let response = seal_ike_sk_cbc(
                IkeHeader {
                    flags: IkeHeader::RESPONSE_FLAG,
                    ..request_header
                },
                12,
                integrity,
                &responder_encryption,
                &responder_integrity,
                &frames,
            )
            .unwrap();
            assert_eq!(session.open_sk(&response).unwrap().1, frames);
        }
    }

    #[tokio::test]
    async fn ike_auth_retransmits_identical_ciphertext_and_accepts_encrypted_reply() {
        exercise_encrypted_reply(false, false, 0).await;
    }

    #[tokio::test]
    async fn peer_ike_delete_is_acknowledged_before_tunnel_closes() {
        exercise_encrypted_reply(false, false, 9).await;
    }

    #[tokio::test]
    async fn local_stop_sends_one_protected_ike_delete_before_closing() {
        exercise_encrypted_reply(false, false, 10).await;
    }

    #[tokio::test]
    async fn local_stop_waits_for_pending_dpd_then_sends_delete() {
        exercise_encrypted_reply(false, false, 11).await;
    }

    #[tokio::test]
    async fn unauthenticated_replies_cannot_complete_authentication() {
        exercise_encrypted_reply(true, false, 0).await;
    }

    #[tokio::test]
    async fn encrypted_but_wrong_responder_auth_is_rejected() {
        exercise_encrypted_reply(false, true, 0).await;
    }

    #[tokio::test]
    async fn final_auth_rejects_missing_unoffered_and_duplicate_child_sa() {
        for mode in 1..=3 {
            tokio::time::timeout(
                Duration::from_secs(3),
                exercise_encrypted_reply(false, false, mode),
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn authenticated_notifications_preserve_peer_failure_and_allow_status() {
        for mode in 4..=6 {
            tokio::time::timeout(
                Duration::from_secs(3),
                exercise_encrypted_reply(false, false, mode),
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn final_auth_rejects_missing_traffic_selectors() {
        tokio::time::timeout(
            Duration::from_secs(3),
            exercise_encrypted_reply(false, false, 7),
        )
        .await
        .unwrap();
    }

    fn esp_proposal(spi: u32) -> PayloadFrame {
        let mut body = vec![0, 0, 0, 32, 1, 3, 4, 2];
        body.extend_from_slice(&spi.to_be_bytes());
        body.extend_from_slice(&[
            3, 0, 0, 12, 1, 0, 0, 20, 0x80, 14, 0, 128, 0, 0, 0, 8, 5, 0, 0, 0,
        ]);
        PayloadFrame {
            kind: 33,
            critical: false,
            body,
        }
    }

    fn selector(kind: u8) -> PayloadFrame {
        PayloadFrame {
            kind,
            critical: false,
            body: vec![
                1, 0, 0, 0, 7, 0, 0, 16, 0, 0, 255, 255, 0, 0, 0, 0, 255, 255, 255, 255,
            ],
        }
    }

    fn config_request() -> PayloadFrame {
        PayloadFrame {
            kind: 47,
            critical: false,
            body: vec![1, 0, 0, 0, 0, 1, 0, 0],
        }
    }

    #[tokio::test]
    async fn final_auth_requires_requested_configuration() {
        tokio::time::timeout(
            Duration::from_secs(3),
            exercise_encrypted_reply(false, false, 8),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn final_auth_ignores_esp_arriving_before_the_authenticated_reply() {
        tokio::time::timeout(
            Duration::from_secs(3),
            exercise_encrypted_reply(false, false, 12),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn final_auth_esp_only_traffic_keeps_the_original_deadline() {
        tokio::time::timeout(
            Duration::from_secs(3),
            exercise_encrypted_reply(true, false, 13),
        )
        .await
        .unwrap();
    }

    async fn exercise_encrypted_reply(invalid_only: bool, bad_auth: bool, child_mode: u8) {
        let peer = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_addr = peer.local_addr().unwrap();
        let initiator_spi = 0x1122_3344_5566_7788;
        let responder_spi = 0x8877_6655_4433_2211;
        let sa_init_header = proletarius_ike::IkeHeader {
            initiator_spi,
            responder_spi,
            next_payload: 0,
            version: 0x20,
            exchange: proletarius_ike::ExchangeType::IkeSaInit,
            flags: proletarius_ike::IkeHeader::RESPONSE_FLAG,
            message_id: 0,
        };
        let key_schedule = SaInitKeySchedule {
            initiator_nonce: vec![6; 32],
            prf_id: 5,
            initiator_spi,
            encryption_transform: 20,
            integrity_transform: None,
            response: SaInitResponse {
                request_packet: vec![3; 28],
                raw_packet: sa_init_header.encode(&[]).unwrap(),
                responder_spi,
                selected_sa: vec![],
                selected_proposal: None,
                dh_group: 14,
                peer_public_key: vec![],
                responder_nonce: vec![7; 32],
            },
            keys: IkeSaKeyMaterial {
                sk_d: vec![1; 32],
                sk_ai: vec![],
                sk_ar: vec![],
                sk_ei: vec![0x55; 20],
                sk_er: vec![0x66; 20],
                sk_pi: vec![2; 32],
                sk_pr: vec![3; 32],
            },
        };
        let mut session = IkeAuthSession::from_sa_init(key_schedule).unwrap();
        let transport = EpdgDatagramTransport::open(
            ProxyMode::Direct(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            "unused.invalid",
            peer_addr.port(),
            None,
        )
        .await
        .unwrap();
        let transport = if matches!(child_mode, 12 | 13) {
            transport.enable_pinned_natt()
        } else {
            transport
        };
        let runtime_peer = std::sync::Arc::new(peer);
        let peer = runtime_peer.clone();
        let peer_task = tokio::spawn(async move {
            let mut buffer = [0; 2048];
            let (first_len, source) = peer.recv_from(&mut buffer).await.unwrap();
            let first = buffer[..first_len].to_vec();
            let (second_len, _) = peer.recv_from(&mut buffer).await.unwrap();
            let second = buffer[..second_len].to_vec();
            assert_eq!(
                first, second,
                "retries must reuse identical ciphertext and IV"
            );
            let (request_header, frames) = open_ike_sk_gcm(&first, 20, &[0x55; 20]).unwrap();
            assert_eq!(request_header.message_id, 1);
            assert_eq!(
                frames,
                vec![
                    PayloadFrame {
                        kind: 35,
                        critical: false,
                        body: b"AKA-RESP".to_vec()
                    },
                    esp_proposal(0x11223344),
                    selector(44),
                    selector(45),
                    config_request()
                ]
            );
            let response_header = proletarius_ike::IkeHeader {
                initiator_spi,
                responder_spi,
                next_payload: 0,
                version: 0x20,
                exchange: proletarius_ike::ExchangeType::IkeAuth,
                flags: proletarius_ike::IkeHeader::RESPONSE_FLAG,
                message_id: 1,
            };
            let response = seal_ike_sk_gcm(
                response_header,
                20,
                &[0x66; 20],
                &[PayloadFrame {
                    kind: 36,
                    critical: false,
                    body: b"\x02\0\0\0epdg.example".to_vec(),
                }],
            )
            .unwrap();
            // Neither a damaged authentication tag nor an unauthenticated
            // changed SPI may consume the pending message ID or close the SA.
            let mut damaged = response.clone();
            *damaged.last_mut().unwrap() ^= 1;
            peer.send_to(&damaged, source).await.unwrap();
            let mut changed_spi = response.clone();
            changed_spi[0] ^= 1;
            peer.send_to(&changed_spi, source).await.unwrap();
            if child_mode == 13 {
                for _ in 0..3 {
                    peer.send_to(&[0x11; 8], source).await.unwrap();
                }
            }
            if !invalid_only {
                peer.send_to(&response, source).await.unwrap();
                let (length, source) = peer.recv_from(&mut buffer).await.unwrap();
                let (mut header, frames) =
                    open_ike_sk_gcm(&buffer[..length], 20, &[0x55; 20]).unwrap();
                assert_eq!(header.message_id, 2);
                let expected = proletarius_ike::compute_eap_auth(
                    5,
                    &[1; 64],
                    &[3; 28],
                    &[7; 32],
                    &[2; 32],
                    b"AKA-RESP",
                )
                .unwrap();
                assert_eq!(&frames[0].body[4..], expected.as_slice());
                let responder_auth = proletarius_ike::compute_eap_auth(
                    5,
                    &[1; 64],
                    &sa_init_header.encode(&[]).unwrap(),
                    &[6; 32],
                    &[3; 32],
                    b"\x02\0\0\0epdg.example",
                )
                .unwrap();
                let mut body = vec![2, 0, 0, 0];
                body.extend_from_slice(&responder_auth);
                if bad_auth {
                    *body.last_mut().unwrap() ^= 1;
                }
                header.flags = proletarius_ike::IkeHeader::RESPONSE_FLAG;
                let mut final_frames = vec![PayloadFrame {
                    kind: 39,
                    critical: false,
                    body,
                }];
                if child_mode != 1 {
                    let mut proposal = esp_proposal(0x55667788);
                    if child_mode == 2 {
                        proposal.body[23] = 64;
                    }
                    final_frames.push(proposal.clone());
                    if child_mode == 3 {
                        final_frames.push(proposal);
                    }
                }
                if (4..=6).contains(&child_mode) {
                    let body = match child_mode {
                        4 => vec![0, 0, 0, 24],   // AUTHENTICATION_FAILED
                        5 => vec![0, 0, 0x40, 0], // INITIAL_CONTACT status
                        _ => vec![3, 4, 0, 24],   // missing declared SPI
                    };
                    final_frames.push(PayloadFrame {
                        kind: 41,
                        critical: false,
                        body,
                    });
                }
                final_frames.extend([selector(44), selector(45)]);
                if child_mode != 8 {
                    final_frames.push(PayloadFrame {
                        kind: 47,
                        critical: false,
                        body: vec![2, 0, 0, 0, 0, 1, 0, 4, 10, 0, 0, 2],
                    });
                }
                if child_mode == 7 {
                    final_frames.retain(|f| f.kind != 45);
                }
                let reply = seal_ike_sk_gcm(header, 20, &[0x66; 20], &final_frames).unwrap();
                if child_mode == 12 {
                    // A prior CHILD_SA, a retransmission, or peer scheduling
                    // can place ESP ahead of the authenticated IKE_AUTH reply
                    // on the shared NAT-T channel. It is not evidence against
                    // the in-flight IKE exchange and must not close the SA.
                    peer.send_to(&[0x11; 8], source).await.unwrap();
                }
                peer.send_to(&reply, source).await.unwrap();
            }
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
                    esp_proposal(0x11223344),
                    selector(44),
                    selector(45),
                    config_request(),
                ],
                2,
                Duration::from_millis(80),
            )
            .await;
        if invalid_only {
            peer_task.await.unwrap();
            assert!(matches!(response, Err(super::IkeAuthError::Timeout)));
            assert_eq!(session.phase(), proletarius_ike::SessionPhase::Closed);
            return;
        }
        let response = response.unwrap();
        assert_eq!(response.header.message_id, 1);
        assert_eq!(response.frames[0].body, b"\x02\0\0\0epdg.example");
        assert_eq!(
            session.phase(),
            proletarius_ike::SessionPhase::Authenticating
        );
        assert!(session.verify_responder_eap_auth(&[1; 64], &[]).is_err());
        let result = session
            .finish_eap_authentication(
                &transport,
                crate::EapHandshakeResult {
                    msk: zeroize::Zeroizing::new(vec![1; 64]),
                    k_aut: zeroize::Zeroizing::new(vec![1; 16]),
                    last_response: response,
                    eap_direct_material: (child_mode == 0 && !bad_auth).then(|| {
                        crate::EapDirectAuthMaterial::new(&[0x44; 8], &[0x11; 16], &[0x22; 16])
                            .unwrap()
                    }),
                },
            )
            .await;
        peer_task.await.unwrap();
        if bad_auth {
            assert!(matches!(
                result,
                Err(super::IkeAuthError::ResponderAuthenticationFailed)
            ));
        } else if (1..=3).contains(&child_mode) {
            assert!(matches!(result, Err(super::IkeAuthError::ChildSa(_))));
        } else if child_mode == 4 {
            assert!(matches!(result, Err(super::IkeAuthError::PeerNotify(24))));
        } else if child_mode == 6 {
            assert!(matches!(result, Err(super::IkeAuthError::MalformedNotify)));
        } else if child_mode == 7 {
            assert!(matches!(
                result,
                Err(super::IkeAuthError::TrafficSelectors(_))
            ));
        } else if child_mode == 8 {
            assert!(matches!(result, Err(super::IkeAuthError::Configuration(_))));
        } else {
            let (mut session, child) = result.unwrap();
            if child_mode == 0 {
                let material = session.take_eap_direct_auth_material().unwrap();
                assert_eq!(material.res(), &[0x44; 8]);
                assert_eq!(material.rand(), &[0x11; 16]);
                assert_eq!(material.autn(), &[0x22; 16]);
                assert!(session.take_eap_direct_auth_material().is_none());
            }
            let probe_header = IkeHeader {
                initiator_spi: session.initiator_spi,
                responder_spi: session.responder_spi,
                next_payload: 46,
                version: 0x20,
                exchange: ExchangeType::Informational,
                flags: 0,
                message_id: 0,
            };
            let probe = seal_ike_sk_gcm(probe_header.clone(), 20, &[0x66; 20], &[]).unwrap();
            let mut corrupt = probe.clone();
            *corrupt.last_mut().unwrap() ^= 1;
            assert!(session.reply_to_peer_liveness(&corrupt).is_err());
            let reply = session.reply_to_peer_liveness(&probe).unwrap();
            assert_eq!(session.reply_to_peer_liveness(&probe).unwrap(), reply);
            let (header, frames) = open_ike_sk_gcm(&reply, 20, &[0x55; 20]).unwrap();
            assert_eq!(
                header.flags,
                IkeHeader::INITIATOR_FLAG | IkeHeader::RESPONSE_FLAG
            );
            assert_eq!(header.message_id, 0);
            assert!(frames.is_empty());
            let gap = seal_ike_sk_gcm(
                IkeHeader {
                    message_id: 2,
                    ..probe_header.clone()
                },
                20,
                &[0x66; 20],
                &[],
            )
            .unwrap();
            assert!(session.reply_to_peer_liveness(&gap).is_err());
            let next = seal_ike_sk_gcm(
                IkeHeader {
                    message_id: 1,
                    ..probe_header
                },
                20,
                &[0x66; 20],
                &[],
            )
            .unwrap();
            assert!(session.reply_to_peer_liveness(&next).is_ok());
            assert!(session.reply_to_peer_liveness(&probe).is_err());
            let outbound = session.begin_liveness(2, Duration::from_secs(1)).unwrap();
            assert!(session.begin_liveness(1, Duration::from_secs(1)).is_err());
            let (mut reply_header, frames) = open_ike_sk_gcm(&outbound, 20, &[0x55; 20]).unwrap();
            assert!(frames.is_empty());
            reply_header.flags = IkeHeader::RESPONSE_FLAG;
            let response_probe = seal_ike_sk_gcm(reply_header, 20, &[0x66; 20], &[]).unwrap();
            let mut bad = response_probe.clone();
            *bad.last_mut().unwrap() ^= 1;
            assert!(session.accept_liveness_response(&bad).is_err());
            assert!(session.retry_liveness().unwrap().is_none());
            session.liveness_probe.as_mut().unwrap().deadline = tokio::time::Instant::now();
            assert_eq!(session.retry_liveness().unwrap().unwrap(), outbound);
            session.accept_liveness_response(&response_probe).unwrap();
            assert!(session.accept_liveness_response(&response_probe).is_err());
            assert_eq!(
                child.configuration.as_ref().unwrap().ipv4_addresses[0],
                Ipv4Addr::new(10, 0, 0, 2)
            );
            let response = child.response.clone();
            assert_eq!(
                (child.selection.local_spi, child.selection.remote_spi),
                (0x11223344, 0x55667788)
            );
            assert_eq!(child.keys.initiator_encryption.len(), 20);
            assert_eq!(child.keys.responder_encryption.len(), 20);
            assert!(child.keys.initiator_integrity.is_empty());
            assert_ne!(
                child.keys.initiator_encryption,
                child.keys.responder_encryption
            );
            assert_eq!(session.phase(), proletarius_ike::SessionPhase::Established);
            if !matches!(child_mode, 0 | 9 | 10 | 11) {
                session.begin_liveness(1, Duration::from_secs(1)).unwrap();
                session.liveness_probe.as_mut().unwrap().deadline = tokio::time::Instant::now();
                assert!(matches!(
                    session.retry_liveness(),
                    Err(super::IkeAuthError::Timeout)
                ));
                assert_eq!(session.phase(), proletarius_ike::SessionPhase::Closed);
                assert!(session.begin_liveness(1, Duration::from_secs(1)).is_err());
            }
            assert!(
                session
                    .verify_responder_eap_auth(
                        &[1; 64],
                        &[response.frames[0].clone(), response.frames[0].clone()]
                    )
                    .is_err()
            );
            let mut peer_keys = proletarius_ike::derive_initial_child_sa_keys(
                5, &[1; 32], &[6; 32], &[7; 32], 20, 0,
            )
            .unwrap();
            let mut peer_receiver = proletarius_ike::esp::EspReceiver::new(
                0x55667788,
                zeroize::Zeroizing::new(std::mem::take(&mut peer_keys.initiator_encryption)),
            )
            .unwrap();
            let mut peer_sender = proletarius_ike::esp::EspSender::new(
                0x11223344,
                zeroize::Zeroizing::new(std::mem::take(&mut peer_keys.responder_encryption)),
            )
            .unwrap();
            let mut plane = child.into_gcm_data_plane().unwrap();
            let mut packet = vec![0; 28];
            packet[0] = 0x45;
            packet[3] = 28;
            packet[9] = 17;
            packet[12..16].copy_from_slice(&[10, 0, 0, 2]);
            packet[16..20].copy_from_slice(&[10, 0, 0, 9]);
            packet[20..24].copy_from_slice(&[0x13, 0xc4, 0x13, 0xc4]);
            packet[25] = 8;
            assert_eq!(
                &*peer_receiver
                    .open(&plane.seal_ip(&packet).unwrap())
                    .unwrap(),
                &packet
            );
            packet[15] = 9;
            packet[19] = 2;
            assert_eq!(
                &*plane.open_esp(&peer_sender.seal(&packet).unwrap()).unwrap(),
                &packet
            );
            packet[19] = 3;
            assert!(matches!(
                plane.open_esp(&peer_sender.seal(&packet).unwrap()),
                Err(crate::ChildDataError::PolicyRejected(_))
            ));
            assert!(matches!(
                plane.seal_ip(&packet),
                Err(crate::ChildDataError::PolicyRejected(_))
            ));
            if matches!(child_mode, 0 | 9 | 10 | 11) {
                let (_out_tx, out_rx) = tokio::sync::mpsc::channel(4);
                let (in_tx, mut in_rx) = tokio::sync::mpsc::channel(4);
                let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
                let cancel_tx = stop_tx.clone();
                let cancellation = if child_mode == 10 {
                    Some(tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        cancel_tx.send(true).unwrap();
                    }))
                } else {
                    None
                };
                let remote = async {
                    let mut buffer = [0; 2048];
                    if child_mode == 10 {
                        let (n, _) = runtime_peer.recv_from(&mut buffer).await.unwrap();
                        let (header, frames) =
                            open_ike_sk_gcm(&buffer[..n], 20, &[0x55; 20]).unwrap();
                        assert_eq!(header.exchange, ExchangeType::Informational);
                        assert_eq!(header.flags, IkeHeader::INITIATOR_FLAG);
                        assert_eq!(
                            frames,
                            [PayloadFrame {
                                kind: 42,
                                critical: false,
                                body: vec![1, 0, 0, 0],
                            }]
                        );
                        assert!(
                            tokio::time::timeout(
                                Duration::from_millis(80),
                                runtime_peer.recv_from(&mut buffer)
                            )
                            .await
                            .is_err(),
                            "local shutdown must not retransmit IKE DELETE"
                        );
                        return;
                    }
                    let (n, source) = runtime_peer.recv_from(&mut buffer).await.unwrap();
                    let (mut header, frames) =
                        open_ike_sk_gcm(&buffer[..n], 20, &[0x55; 20]).unwrap();
                    assert!(frames.is_empty());
                    if child_mode == 11 {
                        stop_tx.send(true).unwrap();
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    header.flags = IkeHeader::RESPONSE_FLAG;
                    let response = seal_ike_sk_gcm(header, 20, &[0x66; 20], &[]).unwrap();
                    let mut bad = response.clone();
                    *bad.last_mut().unwrap() ^= 1;
                    runtime_peer.send_to(&bad, source).await.unwrap();
                    runtime_peer.send_to(&response, source).await.unwrap();
                    if child_mode == 9 {
                        let request_header = IkeHeader {
                            initiator_spi,
                            responder_spi,
                            next_payload: 46,
                            version: 0x20,
                            exchange: ExchangeType::Informational,
                            flags: 0,
                            message_id: 2,
                        };
                        let delete = seal_ike_sk_gcm(
                            request_header,
                            20,
                            &[0x66; 20],
                            &[PayloadFrame {
                                kind: 42,
                                critical: false,
                                body: vec![1, 0, 0, 0],
                            }],
                        )
                        .unwrap();
                        let mut corrupt = delete.clone();
                        *corrupt.last_mut().unwrap() ^= 1;
                        runtime_peer.send_to(&corrupt, source).await.unwrap();
                        runtime_peer.send_to(&delete, source).await.unwrap();
                        let (n, _) = runtime_peer.recv_from(&mut buffer).await.unwrap();
                        let (ack, frames) = open_ike_sk_gcm(&buffer[..n], 20, &[0x55; 20]).unwrap();
                        assert_eq!(ack.message_id, 2);
                        assert_eq!(
                            ack.flags,
                            IkeHeader::INITIATOR_FLAG | IkeHeader::RESPONSE_FLAG
                        );
                        assert!(frames.is_empty());
                        return;
                    }
                    if child_mode == 11 {
                        let (n, _) = runtime_peer.recv_from(&mut buffer).await.unwrap();
                        let (header, frames) =
                            open_ike_sk_gcm(&buffer[..n], 20, &[0x55; 20]).unwrap();
                        assert_eq!(header.exchange, ExchangeType::Informational);
                        assert_eq!(header.flags, IkeHeader::INITIATOR_FLAG);
                        assert_eq!(
                            frames,
                            [PayloadFrame {
                                kind: 42,
                                critical: false,
                                body: vec![1, 0, 0, 0],
                            }]
                        );
                        return;
                    }
                    // A second probe proves the first response was accepted;
                    // stop answering and require an identical retransmission.
                    let (n, _) = runtime_peer.recv_from(&mut buffer).await.unwrap();
                    let first = buffer[..n].to_vec();
                    let (n, _) = runtime_peer.recv_from(&mut buffer).await.unwrap();
                    assert_eq!(first, buffer[..n]);
                };
                let local = session.run_tunnel(
                    plane,
                    transport,
                    out_rx,
                    in_tx,
                    stop_rx,
                    if child_mode == 10 {
                        Duration::from_secs(10)
                    } else {
                        Duration::from_millis(20)
                    },
                    Duration::from_millis(30),
                    2,
                );
                let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
                    tokio::join!(local, remote)
                })
                .await
                .unwrap();
                if let Some(cancellation) = cancellation {
                    cancellation.await.unwrap();
                }
                if child_mode == 9 {
                    assert!(matches!(
                        result,
                        Err(super::IkeAuthError::DataPlane(
                            crate::ChildDataError::PeerDeleted
                        ))
                    ));
                } else if child_mode == 0 {
                    assert!(matches!(result, Err(super::IkeAuthError::Timeout)));
                } else {
                    let stats = result.unwrap();
                    assert_eq!(stats.received_ip, 0);
                    assert_eq!(stats.received_ike, u64::from(child_mode == 11) * 2);
                    assert_eq!(stats.discarded_packets, 0);
                }
                assert!(_out_tx.is_closed());
                assert!(in_rx.recv().await.is_none());
                let mut after = [0; 2048];
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(60),
                        runtime_peer.recv_from(&mut after)
                    )
                    .await
                    .is_err()
                );
            }
        }
    }
}

#[derive(Debug)]
pub enum IkeAuthError {
    DataPlane(crate::ChildDataError),
    Configuration(crate::TunnelConfigError),
    TrafficSelectors(crate::TrafficSelectorError),
    PeerNotify(u16),
    MalformedNotify,
    ChildSa(crate::ChildSaError),
    ResponderAuthenticationFailed,
    KeyDerivation(proletarius_ike::KeyDerivationError),
    InvalidAttempts,
    PlannedReauthentication,
    Timeout,
    UnexpectedPacket,
    Ike(IkeProtocolError),
    Sk(SkPayloadError),
    Transport(DatagramTransportError),
}

impl fmt::Display for IkeAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlannedReauthentication => formatter.write_str("planned IKE reauthentication"),
            Self::DataPlane(error) => error.fmt(formatter),
            Self::Configuration(error) => error.fmt(formatter),
            Self::TrafficSelectors(error) => error.fmt(formatter),
            Self::PeerNotify(code) => write!(
                formatter,
                "IKE peer rejected authentication or CHILD SA (Notify {code})"
            ),
            Self::MalformedNotify => {
                formatter.write_str("IKE_AUTH contains a malformed Notify payload")
            }
            Self::ChildSa(error) => error.fmt(formatter),
            Self::ResponderAuthenticationFailed => {
                formatter.write_str("IKE responder EAP AUTH verification failed")
            }
            Self::KeyDerivation(error) => error.fmt(formatter),
            Self::InvalidAttempts => formatter.write_str("IKE_AUTH needs at least one attempt"),
            Self::Timeout => formatter.write_str("IKE_AUTH timed out after all attempts"),
            Self::UnexpectedPacket => {
                formatter.write_str("IKE_AUTH received ESP on the IKE channel")
            }
            Self::Ike(error) => error.fmt(formatter),
            Self::Sk(error) => error.fmt(formatter),
            Self::Transport(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for IkeAuthError {}

impl From<IkeProtocolError> for IkeAuthError {
    fn from(value: IkeProtocolError) -> Self {
        Self::Ike(value)
    }
}

impl From<SkPayloadError> for IkeAuthError {
    fn from(value: SkPayloadError) -> Self {
        Self::Sk(value)
    }
}

impl From<DatagramTransportError> for IkeAuthError {
    fn from(value: DatagramTransportError) -> Self {
        Self::Transport(value)
    }
}

pub struct IkeAuthSession {
    pub(super) peer_certificate_policy: Option<crate::PeerCertificatePolicy>,
    pub(super) initial_certificate_verified: bool,
    local_initiator: bool,
    ike_offer: Option<Vec<u8>>,
    auth_lifetime: Option<crate::ike_rekey::AuthenticationLifetime>,
    liveness_probe: Option<LivenessProbe>,
    final_auth_verified: bool,
    peer_request_id: u32,
    last_peer_probe: Option<(Vec<u8>, Vec<u8>)>,
    configuration_requested: bool,
    initial_selectors: Vec<PayloadFrame>,
    child_sa_offer: Option<Vec<u8>>,
    child_selection: Option<crate::InitialChildSaSelection>,
    child_selectors: Vec<PayloadFrame>,
    pub(super) prf_id: u16,
    pub(super) sa_init_request: Vec<u8>,
    pub(super) sa_init_response: Vec<u8>,
    pub(super) initiator_nonce: Vec<u8>,
    responder_nonce: Vec<u8>,
    responder_identity: Option<zeroize::Zeroizing<Vec<u8>>>,
    initiator_identity: Option<zeroize::Zeroizing<Vec<u8>>>,
    tracker: SessionTracker,
    initiator_spi: u64,
    responder_spi: u64,
    encryption_transform: u16,
    integrity_transform: Option<u16>,
    pub(super) keys: IkeSaKeyMaterial,
    eap_direct_material: Option<crate::EapDirectAuthMaterial>,
}

struct LivenessProbe {
    packet: Vec<u8>,
    deadline: tokio::time::Instant,
    interval: Duration,
    remaining: u8,
}

impl fmt::Debug for IkeAuthSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IkeAuthSession")
            .field("initiator_spi", &self.initiator_spi)
            .field("responder_spi", &self.responder_spi)
            .field("encryption_transform", &self.encryption_transform)
            .field("integrity_transform", &self.integrity_transform)
            .field("tracker", &self.tracker)
            .field("keys", &"[REDACTED]")
            .field(
                "eap_direct_material",
                &self.eap_direct_material.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IkeAuthResponse {
    pub header: IkeHeader,
    pub frames: Vec<PayloadFrame>,
}

/// AUTH, ESP proposal and traffic selectors verified, but configuration and
/// ESP installation are still required. This is deliberately not an online SA.
#[derive(Debug)]
pub struct PendingChildSa {
    pub configuration: Option<crate::TunnelConfiguration>,
    pub traffic_selectors: crate::ChildTrafficSelectors,
    pub response: IkeAuthResponse,
    pub selection: crate::InitialChildSaSelection,
    pub keys: proletarius_ike::ChildSaKeyMaterial,
}

impl IkeAuthSession {
    /// Promotes a validated SA_INIT result to an IKE_AUTH session, beginning
    /// at message ID 1 and retaining its negotiated keys and transform.
    pub fn from_sa_init(schedule: SaInitKeySchedule) -> Result<Self, IkeAuthError> {
        let (header, _) = IkeHeader::decode(&schedule.response.raw_packet)?;
        if header.exchange != ExchangeType::IkeSaInit
            || header.initiator_spi != schedule.initiator_spi
            || header.responder_spi != schedule.response.responder_spi
        {
            return Err(IkeAuthError::Ike(IkeProtocolError::UnexpectedResponse));
        }
        let tracker = SessionTracker::from_sa_init_response(&header)?;
        match (schedule.encryption_transform, schedule.integrity_transform) {
            (20, None)
                if matches!(schedule.keys.sk_ei.len(), 20 | 36)
                    && schedule.keys.sk_ei.len() == schedule.keys.sk_er.len()
                    && schedule.keys.sk_ai.is_empty()
                    && schedule.keys.sk_ar.is_empty() => {}
            (12, Some(12))
                if matches!(schedule.keys.sk_ei.len(), 16 | 32)
                    && schedule.keys.sk_ei.len() == schedule.keys.sk_er.len()
                    && schedule.keys.sk_ai.len() == 32
                    && schedule.keys.sk_ar.len() == 32 => {}
            (12, Some(2))
                if matches!(schedule.keys.sk_ei.len(), 16 | 32)
                    && schedule.keys.sk_ei.len() == schedule.keys.sk_er.len()
                    && schedule.keys.sk_ai.len() == 20
                    && schedule.keys.sk_ar.len() == 20 => {}
            (12, Some(14))
                if matches!(schedule.keys.sk_ei.len(), 16 | 32)
                    && schedule.keys.sk_ei.len() == schedule.keys.sk_er.len()
                    && schedule.keys.sk_ai.len() == 64
                    && schedule.keys.sk_ar.len() == 64 => {}
            (12, Some(other)) => {
                return Err(SkPayloadError::UnsupportedIntegrity(other).into());
            }
            (other, _) if other != 12 && other != 20 => {
                return Err(SkPayloadError::UnsupportedEncryption(other).into());
            }
            _ => return Err(SkPayloadError::InvalidKeyLength.into()),
        }
        let ike_offer = schedule
            .response
            .selected_proposal
            .as_ref()
            .map(|v| crate::child_rekey::encode(&[v.clone()]))
            .transpose()?;
        Ok(Self {
            local_initiator: true,
            ike_offer,
            auth_lifetime: None,
            liveness_probe: None,
            final_auth_verified: false,
            peer_request_id: 0,
            last_peer_probe: None,
            configuration_requested: false,
            initial_selectors: Vec::new(),
            child_sa_offer: None,
            child_selection: None,
            child_selectors: Vec::new(),
            prf_id: schedule.prf_id,
            sa_init_response: schedule.response.raw_packet,
            initiator_nonce: schedule.initiator_nonce,
            peer_certificate_policy: None,
            initial_certificate_verified: false,
            responder_identity: None,
            sa_init_request: schedule.response.request_packet,
            responder_nonce: schedule.response.responder_nonce,
            initiator_identity: None,
            tracker,
            initiator_spi: header.initiator_spi,
            responder_spi: header.responder_spi,
            encryption_transform: schedule.encryption_transform,
            integrity_transform: schedule.integrity_transform,
            keys: schedule.keys,
            eap_direct_material: None,
        })
    }

    fn local_flag(&self) -> u8 {
        if self.local_initiator {
            IkeHeader::INITIATOR_FLAG
        } else {
            0
        }
    }
    fn peer_flag(&self) -> u8 {
        if self.local_initiator {
            0
        } else {
            IkeHeader::INITIATOR_FLAG
        }
    }
    fn inbound_keys(&self) -> (&[u8], &[u8]) {
        if self.local_initiator {
            (&self.keys.sk_er, &self.keys.sk_ar)
        } else {
            (&self.keys.sk_ei, &self.keys.sk_ai)
        }
    }
    fn outbound_keys(&self) -> (&[u8], &[u8]) {
        if self.local_initiator {
            (&self.keys.sk_ei, &self.keys.sk_ai)
        } else {
            (&self.keys.sk_er, &self.keys.sk_ar)
        }
    }
    /// Transfer only the control authority. ESP keys/counters and selectors stay
    /// with this owner; the captured old control remains until exact retirement.
    fn install_rekey(
        &mut self,
        material: crate::ike_rekey::Material,
        initiator: bool,
    ) -> Result<Self, IkeAuthError> {
        let tracker = SessionTracker::from_rekey(material.spi_i, material.spi_r, initiator)?;
        let old = Self {
            local_initiator: self.local_initiator,
            ike_offer: self.ike_offer.take(),
            auth_lifetime: self.auth_lifetime,
            liveness_probe: self.liveness_probe.take(),
            final_auth_verified: true,
            peer_request_id: self.peer_request_id,
            last_peer_probe: self.last_peer_probe.take(),
            configuration_requested: false,
            initial_selectors: Vec::new(),
            child_sa_offer: None,
            child_selection: None,
            child_selectors: Vec::new(),
            prf_id: self.prf_id,
            sa_init_request: Vec::new(),
            sa_init_response: Vec::new(),
            initiator_nonce: Vec::new(),
            responder_nonce: Vec::new(),
            peer_certificate_policy: None,
            initial_certificate_verified: false,
            responder_identity: None,
            initiator_identity: None,
            tracker: std::mem::replace(&mut self.tracker, tracker),
            initiator_spi: self.initiator_spi,
            responder_spi: self.responder_spi,
            encryption_transform: self.encryption_transform,
            integrity_transform: self.integrity_transform,
            keys: std::mem::replace(&mut self.keys, material.keys),
            eap_direct_material: None,
        };
        self.local_initiator = initiator;
        self.initiator_spi = material.spi_i;
        self.responder_spi = material.spi_r;
        self.ike_offer = Some(material.offer);
        self.peer_request_id = 0;
        Ok(old)
    }
    /// Transfers the verified AKA material to the same generation's IMS
    /// REGISTER owner. This is one-shot; absent capture fails closed upstream.
    pub fn take_eap_direct_auth_material(&mut self) -> Option<crate::EapDirectAuthMaterial> {
        self.eap_direct_material.take()
    }

    pub fn phase(&self) -> SessionPhase {
        self.tracker.phase()
    }

    fn open_sk(&self, packet: &[u8]) -> Result<(IkeHeader, Vec<PayloadFrame>), SkPayloadError> {
        match (self.encryption_transform, self.integrity_transform) {
            (20, None) => open_ike_sk_gcm(packet, 20, &self.inbound_keys().0),
            (12, Some(integrity)) => open_ike_sk_cbc(
                packet,
                12,
                integrity,
                &self.inbound_keys().0,
                &self.inbound_keys().1,
            ),
            (20, Some(integrity)) => Err(SkPayloadError::UnsupportedIntegrity(integrity)),
            (12, None) => Err(SkPayloadError::UnsupportedIntegrity(0)),
            (other, _) => Err(SkPayloadError::UnsupportedEncryption(other)),
        }
    }

    fn seal_sk(
        &self,
        header: IkeHeader,
        frames: &[PayloadFrame],
    ) -> Result<Vec<u8>, SkPayloadError> {
        match (self.encryption_transform, self.integrity_transform) {
            (20, None) => seal_ike_sk_gcm(header, 20, &self.outbound_keys().0, frames),
            (12, Some(integrity)) => seal_ike_sk_cbc(
                header,
                12,
                integrity,
                &self.outbound_keys().0,
                &self.outbound_keys().1,
                frames,
            ),
            (20, Some(integrity)) => Err(SkPayloadError::UnsupportedIntegrity(integrity)),
            (12, None) => Err(SkPayloadError::UnsupportedIntegrity(0)),
            (other, _) => Err(SkPayloadError::UnsupportedEncryption(other)),
        }
    }

    /// Owns both control and ESP futures without detached tasks. Returning or
    /// cancelling this future drops the socket, both sets of keys, and queues.
    /// Only inner IP reaches the consumer; unverified IKE is never health data.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_tunnel(
        self,
        plane: crate::ChildDataPlane,
        transport: EpdgDatagramTransport,
        outbound: tokio::sync::mpsc::Receiver<zeroize::Zeroizing<Vec<u8>>>,
        inbound: tokio::sync::mpsc::Sender<zeroize::Zeroizing<Vec<u8>>>,
        stop: tokio::sync::watch::Receiver<bool>,
        probe_interval: Duration,
        retry_interval: Duration,
        attempts: u8,
    ) -> Result<crate::TunnelRunStats, IkeAuthError> {
        self.run_tunnel_with_options(
            plane,
            transport,
            outbound,
            inbound,
            stop,
            probe_interval,
            retry_interval,
            attempts,
            crate::startup::NativeTunnelOptions::default(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_tunnel_with_options(
        mut self,
        plane: crate::ChildDataPlane,
        transport: EpdgDatagramTransport,
        mut outbound: tokio::sync::mpsc::Receiver<zeroize::Zeroizing<Vec<u8>>>,
        inbound: tokio::sync::mpsc::Sender<zeroize::Zeroizing<Vec<u8>>>,
        mut stop: tokio::sync::watch::Receiver<bool>,
        probe_interval: Duration,
        retry_interval: Duration,
        attempts: u8,
        options: crate::startup::NativeTunnelOptions,
    ) -> Result<crate::TunnelRunStats, IkeAuthError> {
        use crate::{ChildDataError, TunnelCommand, TunnelEvent};
        use tokio::sync::mpsc;
        use tokio::time::{Instant, MissedTickBehavior};
        if self.phase() != SessionPhase::Established || self.liveness_probe.is_some() {
            return Err(IkeAuthError::UnexpectedPacket);
        }
        if probe_interval.is_zero()
            || probe_interval > Duration::from_secs(86400)
            || retry_interval.is_zero()
            || retry_interval > Duration::from_secs(300)
            || attempts == 0
        {
            return Err(IkeAuthError::InvalidAttempts);
        }
        let configuration = plane.configuration().clone();
        let (commands, command_rx) = mpsc::channel(64);
        let (events, mut event_rx) = mpsc::channel(64);
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        let mut shutdown = Some(shutdown);
        // The IKE owner consumes external cancellation first so an established
        // SA can send one protected DELETE over its existing transport. The
        // data plane still has an internal cancellation channel for ownership,
        // but normally exits after processing ShutdownAfterIke.
        let (_plane_stop, plane_stop_rx) = tokio::sync::watch::channel(false);
        let data = plane.run_with_keepalive(
            transport,
            command_rx,
            events,
            shutdown_rx,
            plane_stop_rx,
            options.nat_keepalive,
        );
        tokio::pin!(data);
        let cancelled = async {
            loop {
                if *stop.borrow() || stop.changed().await.is_err() {
                    return;
                }
            }
        };
        tokio::pin!(cancelled);
        let mut tick = tokio::time::interval(retry_interval.min(Duration::from_millis(100)));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut next_probe = Instant::now() + probe_interval;
        let mut reauth_at = crate::ike_rekey::earlier_deadline(
            crate::ike_rekey::reauth_deadline(options.reauth_interval, options.reauth_jitter)?,
            self.auth_lifetime,
        );
        let mut planned_reauth = false;
        let (mut ike_interval, mut child_interval) =
            crate::ike_rekey::intervals(self.auth_lifetime.map_or(0, |v| v.seconds), &options);
        let mut next_child = crate::ike_rekey::deadline(child_interval, options.rekey_jitter)?;
        let mut next_ike = crate::ike_rekey::deadline(ike_interval, options.rekey_jitter)?;
        let mut ike_failures = 0u8;
        let mut old_reply_cache: Option<(IkeAuthSession, Instant)> = None;
        let mut ike_pending: Option<(crate::ike_rekey::Attempt, Vec<u8>, Instant, u8)> = None;
        // Old control authority remains owned independently from the current SA.
        // No new control request is dispatched until this exact retirement ends.
        let mut retired_ike: Option<(IkeAuthSession, Option<(Vec<u8>, Instant, u8)>, Instant)> =
            None;
        let mut child_pending: Option<(crate::child_rekey::Attempt, Vec<u8>, Instant, u8)> = None;
        let mut delete_pending: Option<(Vec<u8>, Instant, u8)> = None;
        let mut retired_child: Option<crate::InitialChildSaSelection> = None;
        // A peer may omit its old-CHILD DELETE. After the same bounded inbound
        // grace, initiate the exact retirement ourselves without stopping new ESP.
        let mut peer_child_retire_at: Option<Instant> = None;
        // One completed SPI pair permits a crossed/late peer DELETE to be
        // acknowledged without targeting the current SA or holding rekey busy.
        let mut completed_child: Option<crate::InitialChildSaSelection> = None;
        let mut shutdown_deadline = None;
        loop {
            if shutdown_deadline.is_none() && reauth_at.is_some_and(|v| Instant::now() >= v) {
                planned_reauth = true;
                shutdown_deadline = Some(Instant::now() + Duration::from_secs(5));
            }
            if let Some(deadline) = shutdown_deadline
                && self.liveness_probe.is_none()
                && child_pending.is_none()
                && delete_pending.is_none()
                && ike_pending.is_none()
                && retired_ike.is_none()
            {
                let packet = self.begin_local_delete()?;
                let sent = shutdown
                    .take()
                    .ok_or(IkeAuthError::UnexpectedPacket)?
                    .send(packet);
                if sent.is_err() {
                    self.tracker.fail_and_close();
                    return Err(IkeAuthError::DataPlane(
                        ChildDataError::EventConsumerUnavailable,
                    ));
                }
                let result = tokio::time::timeout_at(deadline, &mut data)
                    .await
                    .map_err(|_| IkeAuthError::Timeout)
                    .and_then(|result| result.map_err(IkeAuthError::DataPlane));
                self.tracker.fail_and_close();
                return result.and_then(|stats| {
                    if planned_reauth {
                        Err(IkeAuthError::PlannedReauthentication)
                    } else {
                        Ok(stats)
                    }
                });
            }
            tokio::select! {
                biased;
                _ = &mut cancelled, if shutdown_deadline.is_none() => {
                    // Do not overlap an in-flight DPD request: wait briefly for
                    // its authenticated response, then either send DELETE with
                    // the next message ID or fail closed at the deadline.
                    shutdown_deadline = Some(Instant::now() + Duration::from_secs(5));
                },
                _ = tokio::time::sleep_until(shutdown_deadline.unwrap_or(next_probe)), if shutdown_deadline.is_some() => {
                    self.tracker.fail_and_close();
                    return Err(IkeAuthError::Timeout);
                },
                _ = tokio::time::sleep_until(reauth_at.unwrap_or(next_probe)), if shutdown_deadline.is_none() && reauth_at.is_some() => {
                    planned_reauth = true;
                    shutdown_deadline = Some(Instant::now() + Duration::from_secs(5));
                },
                result = &mut data => return result.map_err(IkeAuthError::DataPlane),
                _ = inbound.closed(), if shutdown_deadline.is_none() => return Err(IkeAuthError::DataPlane(ChildDataError::EventConsumerUnavailable)),
                packet = outbound.recv(), if shutdown_deadline.is_none() => {
                    let Some(packet) = packet else { return Err(IkeAuthError::DataPlane(ChildDataError::EventConsumerUnavailable)); };
                    commands.try_send(TunnelCommand::Ip(packet))
                        .map_err(|_| IkeAuthError::DataPlane(ChildDataError::EventConsumerUnavailable))?;
                },
                _ = tick.tick(), if shutdown_deadline.is_none() || self.liveness_probe.is_some() || child_pending.is_some() || delete_pending.is_some() || ike_pending.is_some() || retired_ike.is_some() => {
                    if old_reply_cache.as_ref().is_some_and(|(_,until)|Instant::now()>=*until) {old_reply_cache=None;}
                    if let Some((old,pending,limit))=&mut retired_ike {
                        if let Some((packet,deadline,left))=pending {
                            if Instant::now()>=*deadline {if *left==0 {return Err(IkeAuthError::Timeout);}*left-=1;*deadline=Instant::now()+retry_interval;commands.try_send(TunnelCommand::EncryptedIke(packet.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;}
                        } else if Instant::now()>=*limit {old.tracker.fail_and_close();return Err(IkeAuthError::Timeout);}
                        continue;
                    }
                    if let Some((_,packet,deadline,left))=&mut ike_pending {
                        if Instant::now()>=*deadline {if *left==0 {return Err(IkeAuthError::Timeout);}*left-=1;*deadline=Instant::now()+retry_interval;commands.try_send(TunnelCommand::EncryptedIke(packet.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;}
                        continue;
                    }
                    if let Some((_,packet,deadline,left))=&mut child_pending {
                        if Instant::now()>=*deadline {if *left==0 {return Err(IkeAuthError::Timeout);}*left-=1;*deadline=Instant::now()+retry_interval;commands.try_send(TunnelCommand::EncryptedIke(packet.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;}continue;
                    }
                    if let Some((packet,deadline,left))=&mut delete_pending {
                        if Instant::now()>=*deadline {if *left==0 {return Err(IkeAuthError::Timeout);}*left-=1;*deadline=Instant::now()+retry_interval;commands.try_send(TunnelCommand::EncryptedIke(packet.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;}continue;
                    }
                    if shutdown_deadline.is_none() && self.liveness_probe.is_none() && peer_child_retire_at.is_some_and(|v|Instant::now()>=v) {
                        let old=retired_child.as_ref().ok_or(IkeAuthError::UnexpectedPacket)?;
                        let id=self.tracker.begin_request(ExchangeType::Informational)?;
                        let mut body=vec![3,4,0,1];body.extend_from_slice(&old.local_spi.to_be_bytes());
                        let packet=self.seal_sk(IkeHeader{initiator_spi:self.initiator_spi,responder_spi:self.responder_spi,next_payload:46,version:0x20,exchange:ExchangeType::Informational,flags:self.local_flag(),message_id:id},&[PayloadFrame{kind:42,critical:false,body}])?;
                        commands.try_send(TunnelCommand::EncryptedIke(packet.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                        delete_pending=Some((packet,Instant::now()+retry_interval,attempts.saturating_sub(1)));peer_child_retire_at=None;continue;
                    }
                    if shutdown_deadline.is_none() && self.liveness_probe.is_none() && child_pending.is_none() && delete_pending.is_none() && retired_child.is_none() && next_ike.is_some_and(|v|Instant::now()>=v) {
                        let old_spi=if self.local_initiator {self.initiator_spi}else{self.responder_spi};
                        let attempt=crate::ike_rekey::begin(self.ike_offer.as_deref().ok_or(IkeAuthError::UnexpectedPacket)?,old_spi)?;
                        let id=self.tracker.begin_request(ExchangeType::CreateChildSa)?;
                        let packet=self.seal_sk(IkeHeader{initiator_spi:self.initiator_spi,responder_spi:self.responder_spi,next_payload:46,version:0x20,exchange:ExchangeType::CreateChildSa,flags:self.local_flag(),message_id:id},&attempt.frames)?;
                        commands.try_send(TunnelCommand::EncryptedIke(packet.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                        ike_pending=Some((attempt,packet,Instant::now()+retry_interval,attempts.saturating_sub(1)));continue;
                    }
                    if shutdown_deadline.is_none() && self.liveness_probe.is_none() && retired_child.is_none() && next_child.is_some_and(|v|Instant::now()>=v) {
                        let old=self.child_selection.as_ref().ok_or(IkeAuthError::UnexpectedPacket)?;
                        let attempt=crate::child_rekey::begin(self.child_sa_offer.as_deref().ok_or(IkeAuthError::UnexpectedPacket)?,&self.child_selectors,old.local_spi,options.rekey_pfs)?;
                        let id=self.tracker.begin_request(ExchangeType::CreateChildSa)?;
                        let packet=self.seal_sk(IkeHeader{initiator_spi:self.initiator_spi,responder_spi:self.responder_spi,next_payload:46,version:0x20,exchange:ExchangeType::CreateChildSa,flags:self.local_flag(),message_id:id},&attempt.frames)?;
                        commands.try_send(TunnelCommand::EncryptedIke(packet.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                        child_pending=Some((attempt,packet,Instant::now()+retry_interval,attempts.saturating_sub(1)));continue;
                    }
                    let packet = if self.liveness_probe.is_some() {
                        self.retry_liveness()?
                    } else if !options.disable_dpd && Instant::now() >= next_probe {
                        Some(self.begin_liveness(attempts, retry_interval)?)
                    } else { None };
                    if let Some(packet) = packet {
                        commands.try_send(TunnelCommand::EncryptedIke(packet))
                            .map_err(|_| IkeAuthError::DataPlane(ChildDataError::EventConsumerUnavailable))?;
                    }
                },
                event = event_rx.recv() => {
                    let Some(event) = event else { return (&mut data).await.map_err(IkeAuthError::DataPlane).and_then(|stats|if planned_reauth {Err(IkeAuthError::PlannedReauthentication)}else{Ok(stats)}); };
                    match event {
                        TunnelEvent::Ip(packet) => {
                            if shutdown_deadline.is_none() {
                                inbound.try_send(packet)
                                    .map_err(|_| IkeAuthError::DataPlane(ChildDataError::EventConsumerUnavailable))?;
                            }
                        },
                        TunnelEvent::UnauthenticatedIke(packet) => {
                            if let Some((old,_))=&old_reply_cache && let Some((request,response))=&old.last_peer_probe && request==&packet {
                                commands.try_send(TunnelCommand::EncryptedIke(response.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;continue;
                            }
                            if let Some((old,pending,limit))=&mut retired_ike {
                                if let Ok((header,frames))=old.open_sk(&packet) {
                                    if header.initiator_spi==old.initiator_spi && header.responder_spi==old.responder_spi && header.flags&IkeHeader::INITIATOR_FLAG==old.peer_flag() {
                                        if let Some((request,response))=&old.last_peer_probe && request==&packet {commands.try_send(TunnelCommand::EncryptedIke(response.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;continue;}
                                        if header.exchange==ExchangeType::Informational && header.is_response() && frames.is_empty() && pending.is_some() {
                                            if pending.as_ref().is_some_and(|(_,deadline,left)|*left==0&&Instant::now()>=*deadline) {return Err(IkeAuthError::Timeout);}
                                            if old.tracker.accept_response(&header).ok()==Some(ResponseDisposition::Accepted) {retired_ike=None;ike_failures=0;next_probe=Instant::now()+probe_interval;next_ike=crate::ike_rekey::deadline(ike_interval,options.rekey_jitter)?;}continue;
                                        }
                                        if header.exchange==ExchangeType::Informational && !header.is_response() && frames.len()==1 && frames[0].kind==42 && frames[0].body==[1,0,0,0] {
                                            if pending.is_none()&&Instant::now()>=*limit {return Err(IkeAuthError::Timeout);}
                                            let Ok(response)=old.reply_to_peer_request(&packet,true) else {continue;};
                                            let (sent,confirmation)=tokio::sync::oneshot::channel();commands.try_send(TunnelCommand::ConfirmedIke{packet:response,sent}).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                            tokio::select! {result=confirmation=>result.map_err(|_|IkeAuthError::UnexpectedPacket)?,result=&mut data=>{result.map_err(IkeAuthError::DataPlane)?;return Err(IkeAuthError::UnexpectedPacket);}}
                                            let mut completed=retired_ike.take().ok_or(IkeAuthError::UnexpectedPacket)?.0;completed.tracker.fail_and_close();old_reply_cache=Some((completed,Instant::now()+Duration::from_secs(3)));ike_failures=0;next_probe=Instant::now()+probe_interval;next_ike=crate::ike_rekey::deadline(ike_interval,options.rekey_jitter)?;continue;
                                        }
                                        // Authenticated old control cannot mutate the new SA or ESP.
                                        continue;
                                    }
                                }
                            }
                            let Ok((header, frames)) = self.open_sk(&packet) else { continue; };
                            if header.initiator_spi != self.initiator_spi || header.responder_spi != self.responder_spi
                                || header.flags & IkeHeader::INITIATOR_FLAG != self.peer_flag() { continue; }
                            if let Some((request,response))=&self.last_peer_probe && request==&packet {
                                commands.try_send(TunnelCommand::EncryptedIke(response.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;continue;
                            }
                            if header.exchange==ExchangeType::CreateChildSa && header.is_response() && ike_pending.is_some() {
                                let (attempt,request,deadline,left)=ike_pending.as_ref().ok_or(IkeAuthError::UnexpectedPacket)?;
                                if *left==0 && Instant::now()>=*deadline {return Err(IkeAuthError::Timeout);}
                                if IkeHeader::decode(request)?.0.message_id!=header.message_id {continue;}
                                if let Some(code)=frames.iter().filter(|v|v.kind==41&&v.body.len()>=4).map(|v|u16::from_be_bytes([v.body[2],v.body[3]])).find(|v|*v<16384) {
                                    if self.tracker.accept_response(&header)?!=ResponseDisposition::Accepted {continue;}
                                    ike_pending=None;ike_failures+=1;
                                    if ike_failures>=2 {return Err(IkeAuthError::PeerNotify(code));}
                                    next_ike=Some(Instant::now()+Duration::from_secs(60));continue;
                                }
                                let peer_spi=if self.local_initiator {self.responder_spi}else{self.initiator_spi};
                                let material=crate::ike_rekey::finish(attempt,&frames,&self.keys.sk_d,peer_spi,self.keys.sk_ei.len(),self.keys.sk_ai.len())?;
                                if self.tracker.accept_response(&header)?!=ResponseDisposition::Accepted {continue;}
                                let mut old=self.install_rekey(material,true)?;ike_pending=None;
                                let packet=old.begin_local_delete()?;
                                commands.try_send(TunnelCommand::EncryptedIke(packet.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                retired_ike=Some((old,Some((packet,Instant::now()+retry_interval,attempts.saturating_sub(1))),Instant::now()+Duration::from_secs(5)));continue;
                            }
                            if header.exchange==ExchangeType::CreateChildSa && !header.is_response() {
                                if let Some((request,response))=&self.last_peer_probe && request==&packet {commands.try_send(TunnelCommand::EncryptedIke(response.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;continue;}
                                if header.message_id!=self.peer_request_id {continue;}
                                let old=self.child_selection.as_ref().ok_or(IkeAuthError::UnexpectedPacket)?.clone();
                                if child_pending.is_some()||delete_pending.is_some()||retired_child.is_some()||self.liveness_probe.is_some()||ike_pending.is_some()||retired_ike.is_some()||shutdown_deadline.is_some() {
                                    let response=self.seal_sk(IkeHeader{flags:self.local_flag()|IkeHeader::RESPONSE_FLAG,..header},&[PayloadFrame{kind:41,critical:false,body:vec![0,0,0,43]}])?;
                                    commands.try_send(TunnelCommand::EncryptedIke(response.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                    self.peer_request_id=self.peer_request_id.checked_add(1).ok_or(IkeAuthError::UnexpectedPacket)?;self.last_peer_probe=Some((packet,response));continue;
                                }
                                let is_ike=frames.iter().find(|v|v.kind==33).and_then(|v|proletarius_ike::decode_sa_proposals(&v.body).ok()).is_some_and(|v|v.len()==1&&v[0].protocol==1);
                                if is_ike {
                                    let local_spi=if self.local_initiator {self.initiator_spi}else{self.responder_spi};
                                    let peer_spi=if self.local_initiator {self.responder_spi}else{self.initiator_spi};
                                    let (response_frames,material)=crate::ike_rekey::respond(self.ike_offer.as_deref().ok_or(IkeAuthError::UnexpectedPacket)?,&frames,&self.keys.sk_d,local_spi,peer_spi,self.keys.sk_ei.len(),self.keys.sk_ai.len())?;
                                    let response=self.seal_sk(IkeHeader{flags:self.local_flag()|IkeHeader::RESPONSE_FLAG,..header},&response_frames)?;
                                    let (sent,confirmation)=tokio::sync::oneshot::channel();commands.try_send(TunnelCommand::ConfirmedIke{packet:response.clone(),sent}).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                    tokio::select! {result=confirmation=>result.map_err(|_|IkeAuthError::UnexpectedPacket)?,result=&mut data=>{result.map_err(IkeAuthError::DataPlane)?;return Err(IkeAuthError::UnexpectedPacket);}}
                                    self.peer_request_id=self.peer_request_id.checked_add(1).ok_or(IkeAuthError::UnexpectedPacket)?;self.last_peer_probe=Some((packet,response));
                                    let old=self.install_rekey(material,false)?;
                                    retired_ike=Some((old,None,Instant::now()+Duration::from_secs(5)));continue;
                                }
                                let (response_frames,material)=crate::child_rekey::respond(&frames,self.child_sa_offer.as_deref().ok_or(IkeAuthError::UnexpectedPacket)?,&self.child_selectors,&old,options.rekey_pfs,self.prf_id,&self.keys.sk_d)?;
                                let response=self.seal_sk(IkeHeader{flags:self.local_flag()|IkeHeader::RESPONSE_FLAG,..header},&response_frames)?;
                                let selected=material.selection.clone();let selectors=material.selector_frames;
                                let replacement=PendingChildSa{configuration:Some(configuration.clone()),traffic_selectors:material.selectors,response:IkeAuthResponse{header,frames:response_frames},selection:selected.clone(),keys:material.keys}.into_data_plane().map_err(IkeAuthError::DataPlane)?;
                                // Confirm the exact response write before making the new outbound SA active.
                                let (sent,confirmation)=tokio::sync::oneshot::channel();commands.try_send(TunnelCommand::ConfirmedIke{packet:response.clone(),sent}).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                tokio::select! {result=confirmation=>result.map_err(|_|IkeAuthError::UnexpectedPacket)?,result=&mut data=>{result.map_err(IkeAuthError::DataPlane)?;return Err(IkeAuthError::UnexpectedPacket);}}
                                let (installed,confirmation)=tokio::sync::oneshot::channel();commands.try_send(TunnelCommand::ReplaceChild{plane:Box::new(replacement),installed}).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                tokio::select! {result=confirmation=>result.map_err(|_|IkeAuthError::UnexpectedPacket)?,result=&mut data=>{result.map_err(IkeAuthError::DataPlane)?;return Err(IkeAuthError::UnexpectedPacket);}}
                                self.child_selection=Some(selected);self.child_selectors=selectors;retired_child=Some(old);peer_child_retire_at=Some(Instant::now()+crate::child_data::OLD_CHILD_GRACE);next_child=crate::ike_rekey::deadline(child_interval,options.rekey_jitter)?;
                                self.peer_request_id=self.peer_request_id.checked_add(1).ok_or(IkeAuthError::UnexpectedPacket)?;self.last_peer_probe=Some((packet,response));continue;
                            }
                            if header.exchange==ExchangeType::CreateChildSa && header.is_response() {
                                let Some((attempt,pending_packet,_,_))=child_pending.as_ref() else {continue;};
                                if IkeHeader::decode(pending_packet)?.0.message_id!=header.message_id {continue;}
                                let old=self.child_selection.as_ref().ok_or(IkeAuthError::UnexpectedPacket)?.clone();
                                let material=crate::child_rekey::finish(attempt,&frames,self.prf_id,&self.keys.sk_d,&old)?;
                                if self.tracker.accept_response(&header)?!=ResponseDisposition::Accepted {continue;}
                                let selected=material.selection.clone();let selectors=material.selector_frames;
                                let replacement=PendingChildSa{configuration:Some(configuration.clone()),traffic_selectors:material.selectors,response:IkeAuthResponse{header:header.clone(),frames:frames.clone()},selection:selected.clone(),keys:material.keys}.into_data_plane().map_err(IkeAuthError::DataPlane)?;
                                let (installed,confirmation)=tokio::sync::oneshot::channel();
                                commands.try_send(TunnelCommand::ReplaceChild{plane:Box::new(replacement),installed}).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                tokio::select! {result=confirmation=>result.map_err(|_|IkeAuthError::UnexpectedPacket)?,result=&mut data=>{result.map_err(IkeAuthError::DataPlane)?;return Err(IkeAuthError::UnexpectedPacket);}}
                                self.child_selection=Some(selected);self.child_selectors=selectors;retired_child=Some(old.clone());child_pending=None;
                                let id=self.tracker.begin_request(ExchangeType::Informational)?;
                                let mut body=vec![3,4,0,1];body.extend_from_slice(&old.local_spi.to_be_bytes());
                                let packet=self.seal_sk(IkeHeader{initiator_spi:self.initiator_spi,responder_spi:self.responder_spi,next_payload:46,version:0x20,exchange:ExchangeType::Informational,flags:self.local_flag(),message_id:id},&[PayloadFrame{kind:42,critical:false,body}])?;
                                commands.try_send(TunnelCommand::EncryptedIke(packet.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                delete_pending=Some((packet,Instant::now()+retry_interval,attempts.saturating_sub(1)));next_child=crate::ike_rekey::deadline(child_interval,options.rekey_jitter)?;continue;
                            }
                            if header.exchange==ExchangeType::Informational && !header.is_response() && frames.len()==1 && frames[0].kind==42 && frames[0].body.first()==Some(&3) {
                                let body=&frames[0].body;
                                if body.len()!=8||body[1..4]!=[4,0,1] {return Err(IkeAuthError::UnexpectedPacket);}
                                let deleted=u32::from_be_bytes(body[4..8].try_into().map_err(|_|IkeAuthError::UnexpectedPacket)?);
                                if let Some((request,response))=&self.last_peer_probe && request==&packet {commands.try_send(TunnelCommand::EncryptedIke(response.clone())).map_err(|_|IkeAuthError::UnexpectedPacket)?;continue;}
                                if header.message_id!=self.peer_request_id {continue;}
                                // A later replacement may reuse an already freed SPI.
                                // The bounded completed cache must never mask a DELETE
                                // that now names the current CHILD pair.
                                if self.child_selection.as_ref().is_some_and(|v|v.remote_spi==deleted) {return Err(IkeAuthError::UnexpectedPacket);}
                                let retiring=retired_child.as_ref().is_some_and(|v|v.remote_spi==deleted);
                                let old=retired_child.as_ref().filter(|v|v.remote_spi==deleted).or_else(||completed_child.as_ref().filter(|v|v.remote_spi==deleted)).ok_or(IkeAuthError::UnexpectedPacket)?;
                                let mut reply=vec![3,4,0,1];reply.extend_from_slice(&old.local_spi.to_be_bytes());
                                let response=self.seal_sk(IkeHeader{flags:self.local_flag()|IkeHeader::RESPONSE_FLAG,..header},&[PayloadFrame{kind:42,critical:false,body:reply}])?;
                                let (sent,confirmation)=tokio::sync::oneshot::channel();commands.try_send(TunnelCommand::ConfirmedIke{packet:response.clone(),sent}).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                tokio::select! {result=confirmation=>result.map_err(|_|IkeAuthError::UnexpectedPacket)?,result=&mut data=>{result.map_err(IkeAuthError::DataPlane)?;return Err(IkeAuthError::UnexpectedPacket);}}
                                if retiring {
                                    let (retired,confirmation)=tokio::sync::oneshot::channel();
                                    commands.try_send(TunnelCommand::RetireOldChild{retired}).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                    tokio::select! {result=confirmation=>result.map_err(|_|IkeAuthError::UnexpectedPacket)?,result=&mut data=>{result.map_err(IkeAuthError::DataPlane)?;return Err(IkeAuthError::UnexpectedPacket);}}
                                    peer_child_retire_at=None;
                                    if delete_pending.is_none() {completed_child=retired_child.take();}
                                }
                                self.peer_request_id=self.peer_request_id.checked_add(1).ok_or(IkeAuthError::UnexpectedPacket)?;self.last_peer_probe=Some((packet,response));continue;
                            }
                            if header.exchange==ExchangeType::Informational && header.is_response() && delete_pending.is_some() {
                                if IkeHeader::decode(&delete_pending.as_ref().ok_or(IkeAuthError::UnexpectedPacket)?.0)?.0.message_id!=header.message_id {continue;}
                                if !frames.is_empty() {
                                    let old=retired_child.as_ref().ok_or(IkeAuthError::UnexpectedPacket)?;
                                    let mut expected=vec![3,4,0,1];expected.extend_from_slice(&old.remote_spi.to_be_bytes());
                                    if frames.len()!=1||frames[0].kind!=42||frames[0].body!=expected{return Err(IkeAuthError::UnexpectedPacket);}
                                }
                                if self.tracker.accept_response(&header)?==ResponseDisposition::Accepted {
                                    let (retired,confirmation)=tokio::sync::oneshot::channel();
                                    commands.try_send(TunnelCommand::RetireOldChild{retired}).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                    tokio::select! {result=confirmation=>result.map_err(|_|IkeAuthError::UnexpectedPacket)?,result=&mut data=>{result.map_err(IkeAuthError::DataPlane)?;return Err(IkeAuthError::UnexpectedPacket);}}
                                    delete_pending=None;completed_child=retired_child.take();peer_child_retire_at=None;
                                }
                                continue;
                            }
                            if header.exchange==ExchangeType::Informational && !header.is_response() && crate::ike_rekey::auth_lifetime(&frames)?.is_some() {
                                if header.message_id!=self.peer_request_id {continue;}
                                if frames.iter().any(|v|v.kind!=41 || v.body[2..4]!=16403u16.to_be_bytes()) {return Err(IkeAuthError::UnexpectedPacket);}
                                self.auth_lifetime=Some(crate::ike_rekey::AuthenticationLifetime::received(crate::ike_rekey::auth_lifetime(&frames)?.ok_or(IkeAuthError::UnexpectedPacket)?));
                                reauth_at=crate::ike_rekey::earlier_deadline(reauth_at,self.auth_lifetime);
                                (ike_interval,child_interval)=crate::ike_rekey::intervals(self.auth_lifetime.map_or(0, |v|v.seconds),&options);
                                next_ike=crate::ike_rekey::deadline(ike_interval,options.rekey_jitter)?;next_child=crate::ike_rekey::deadline(child_interval,options.rekey_jitter)?;
                                let response=self.seal_sk(IkeHeader{flags:self.local_flag()|IkeHeader::RESPONSE_FLAG,..header},&[])?;
                                // Lifetime zero can immediately close the data plane.
                                // Confirm this protected response was written first.
                                let (sent,confirmation)=tokio::sync::oneshot::channel();
                                commands.try_send(TunnelCommand::ConfirmedIke{packet:response.clone(),sent}).map_err(|_|IkeAuthError::UnexpectedPacket)?;
                                tokio::select! {result=confirmation=>result.map_err(|_|IkeAuthError::UnexpectedPacket)?,result=&mut data=>{result.map_err(IkeAuthError::DataPlane)?;return Err(IkeAuthError::UnexpectedPacket);}}
                                self.peer_request_id=self.peer_request_id.checked_add(1).ok_or(IkeAuthError::UnexpectedPacket)?;self.last_peer_probe=Some((packet,response));continue;
                            }
                            let deletes_ike = frames.len() == 1 && frames[0].kind == 42
                                && frames[0].body == [1, 0, 0, 0] && !header.is_response();
                            // Unsupported CHILD DELETE/rekey/Notify must not be
                            // acknowledged as a successful empty probe.
                            if header.exchange != ExchangeType::Informational || (!frames.is_empty() && !deletes_ike) {
                                return Err(IkeAuthError::UnexpectedPacket);
                            }
                            if deletes_ike {
                                let Ok(reply) = self.reply_to_peer_request(&packet, true) else { continue; };
                                commands.try_send(TunnelCommand::CloseAfterIke(reply))
                                    .map_err(|_| IkeAuthError::DataPlane(ChildDataError::EventConsumerUnavailable))?;
                                let deadline = shutdown_deadline
                                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(5));
                                return tokio::time::timeout_at(deadline, &mut data).await
                                    .map_err(|_| IkeAuthError::Timeout)?.map_err(IkeAuthError::DataPlane);
                            }
                            if header.is_response() {
                                match self.accept_liveness_response(&packet) {
                                    Ok(()) => next_probe = Instant::now() + probe_interval,
                                    Err(IkeAuthError::Timeout) => return Err(IkeAuthError::Timeout),
                                    Err(_) => {}, // old or unrelated response cannot reset the timer
                                }
                            } else if let Ok(reply) = self.reply_to_peer_liveness(&packet) {
                                commands.try_send(TunnelCommand::EncryptedIke(reply))
                                    .map_err(|_| IkeAuthError::DataPlane(ChildDataError::EventConsumerUnavailable))?;
                            }
                        },
                    }
                },
            }
        }
    }

    /// Begins local IKE SA deletion after authentication. The returned packet
    /// is already protected and must be sent once over the session transport;
    /// callers must not fall back to a fresh socket when delivery fails.
    fn begin_local_delete(&mut self) -> Result<Vec<u8>, IkeAuthError> {
        self.tracker.begin_delete()?;
        let message_id = match self.tracker.begin_request(ExchangeType::Informational) {
            Ok(message_id) => message_id,
            Err(error) => {
                self.tracker.fail_and_close();
                return Err(error.into());
            }
        };
        let header = IkeHeader {
            initiator_spi: self.initiator_spi,
            responder_spi: self.responder_spi,
            next_payload: 0,
            version: 0x20,
            exchange: ExchangeType::Informational,
            flags: self.local_flag(),
            message_id,
        };
        let delete = PayloadFrame {
            kind: 42,
            critical: false,
            // Protocol ID IKE, zero SPI size and zero SPIs (RFC 7296 3.11).
            body: vec![1, 0, 0, 0],
        };
        match self.seal_sk(header, &[delete]) {
            Ok(packet) => Ok(packet),
            Err(error) => {
                self.tracker.fail_and_close();
                Err(error.into())
            }
        }
    }

    /// Produces ciphertext for the existing transport command queue. The
    /// owner must poll retry_liveness and tear down ESP when this SA closes.
    pub fn begin_liveness(
        &mut self,
        attempts: u8,
        interval: Duration,
    ) -> Result<Vec<u8>, IkeAuthError> {
        if attempts == 0 || interval.is_zero() || interval > Duration::from_secs(300) {
            return Err(IkeAuthError::InvalidAttempts);
        }
        let message_id = self.tracker.begin_request(ExchangeType::Informational)?;
        let header = IkeHeader {
            initiator_spi: self.initiator_spi,
            responder_spi: self.responder_spi,
            next_payload: 0,
            version: 0x20,
            exchange: ExchangeType::Informational,
            flags: self.local_flag(),
            message_id,
        };
        let packet = match self.seal_sk(header, &[]) {
            Ok(packet) => packet,
            Err(error) => {
                self.tracker.fail_and_close();
                return Err(error.into());
            }
        };
        self.liveness_probe = Some(LivenessProbe {
            packet: packet.clone(),
            deadline: tokio::time::Instant::now() + interval,
            interval,
            remaining: attempts - 1,
        });
        Ok(packet)
    }

    pub fn retry_liveness(&mut self) -> Result<Option<Vec<u8>>, IkeAuthError> {
        let Some(probe) = &mut self.liveness_probe else {
            return Ok(None);
        };
        if tokio::time::Instant::now() < probe.deadline {
            return Ok(None);
        }
        if probe.remaining == 0 {
            self.liveness_probe = None;
            self.tracker.fail_and_close();
            return Err(IkeAuthError::Timeout);
        }
        probe.remaining -= 1;
        probe.deadline = tokio::time::Instant::now() + probe.interval;
        Ok(Some(probe.packet.clone()))
    }

    pub fn accept_liveness_response(&mut self, packet: &[u8]) -> Result<(), IkeAuthError> {
        let Some(probe) = &self.liveness_probe else {
            return Err(IkeAuthError::UnexpectedPacket);
        };
        // Late responses cannot resurrect an expired final attempt.
        if probe.remaining == 0 && tokio::time::Instant::now() >= probe.deadline {
            self.liveness_probe = None;
            self.tracker.fail_and_close();
            return Err(IkeAuthError::Timeout);
        }
        let (header, frames) = self.open_sk(packet)?;
        if header.exchange != ExchangeType::Informational || !frames.is_empty() {
            return Err(IkeAuthError::UnexpectedPacket);
        }
        if self.tracker.accept_response(&header)? != ResponseDisposition::Accepted {
            return Err(IkeAuthError::UnexpectedPacket);
        }
        self.liveness_probe = None;
        Ok(())
    }

    /// Replies to a peer liveness probe without reading the socket or claiming
    /// IMS readiness. The data-plane owner routes UnauthenticatedIke here and
    /// queues the returned ciphertext as EncryptedIke. Other control exchanges
    /// must go to their own handlers, never be acknowledged as empty probes.
    pub fn reply_to_peer_liveness(&mut self, packet: &[u8]) -> Result<Vec<u8>, IkeAuthError> {
        self.reply_to_peer_request(packet, false)
    }

    fn reply_to_peer_request(
        &mut self,
        packet: &[u8],
        deletes_ike: bool,
    ) -> Result<Vec<u8>, IkeAuthError> {
        if !self.final_auth_verified || self.phase() == SessionPhase::Closed {
            return Err(IkeAuthError::UnexpectedPacket);
        }
        let (header, frames) = self.open_sk(packet)?;
        if header.initiator_spi != self.initiator_spi
            || header.responder_spi != self.responder_spi
            || header.flags & (IkeHeader::INITIATOR_FLAG | IkeHeader::RESPONSE_FLAG)
                != self.peer_flag()
            || header.exchange != ExchangeType::Informational
            || if deletes_ike {
                frames.len() != 1 || frames[0].kind != 42 || frames[0].body != [1, 0, 0, 0]
            } else {
                !frames.is_empty()
            }
        {
            return Err(IkeAuthError::UnexpectedPacket);
        }
        if let Some((previous, response)) = &self.last_peer_probe
            && previous == packet
        {
            return Ok(response.clone());
        }
        if header.message_id != self.peer_request_id {
            return Err(IkeAuthError::UnexpectedPacket);
        }
        let next = self
            .peer_request_id
            .checked_add(1)
            .ok_or(IkeAuthError::UnexpectedPacket)?;
        let response = self.seal_sk(
            IkeHeader {
                flags: self.local_flag() | IkeHeader::RESPONSE_FLAG,
                ..header
            },
            &[],
        )?;
        self.peer_request_id = next;
        self.last_peer_probe = Some((packet.to_vec(), response.clone()));
        Ok(response)
    }

    /// Sends one encrypted IKE_AUTH round. Every retransmission reuses the
    /// exact same packet/IV; intermediate EAP rounds remain Authenticating.
    pub async fn exchange(
        &mut self,
        transport: &EpdgDatagramTransport,
        frames: &[PayloadFrame],
        attempts: u8,
        attempt_timeout: Duration,
    ) -> Result<IkeAuthResponse, IkeAuthError> {
        if attempts == 0 {
            return Err(IkeAuthError::InvalidAttempts);
        }
        let message_id = self.tracker.begin_request(ExchangeType::IkeAuth)?;
        if message_id == 1 {
            self.configuration_requested = frames.iter().any(|f| f.kind == 47);
            self.initial_selectors = frames
                .iter()
                .filter(|f| matches!(f.kind, 44 | 45))
                .cloned()
                .collect();
            let mut offers = frames.iter().filter(|frame| frame.kind == 33);
            self.child_sa_offer = offers.next().map(|frame| frame.body.clone());
            if offers.next().is_some() {
                self.tracker.fail_and_close();
                return Err(IkeAuthError::ChildSa(
                    crate::ChildSaError::UnofferedProposal,
                ));
            }
            let mut identities = frames.iter().filter(|frame| frame.kind == 35);
            self.initiator_identity = identities
                .next()
                .map(|frame| zeroize::Zeroizing::new(frame.body.clone()));
            if identities.next().is_some() {
                self.tracker.fail_and_close();
                return Err(IkeAuthError::UnexpectedPacket);
            }
        }
        let request_header = IkeHeader {
            initiator_spi: self.initiator_spi,
            responder_spi: self.responder_spi,
            next_payload: 0,
            version: 0x20,
            exchange: ExchangeType::IkeAuth,
            flags: self.local_flag(),
            message_id,
        };
        let request = match self.seal_sk(request_header, frames) {
            Ok(packet) => packet,
            Err(error) => {
                self.tracker.fail_and_close();
                return Err(error.into());
            }
        };

        for _ in 0..attempts {
            if let Err(error) = transport.send_ike(&request).await {
                self.tracker.fail_and_close();
                return Err(error.into());
            }
            let response = timeout(attempt_timeout, async {
                loop {
                    match transport.receive_packet().await? {
                        EpdgPacket::Ike(packet) => {
                            // Do not commit message IDs from unauthenticated
                            // headers. Invalid datagrams consume only the
                            // existing attempt deadline, never a new deadline.
                            let (header, frames) = match self.open_sk(&packet) {
                                Ok(decoded) => decoded,
                                Err(
                                    SkPayloadError::AuthenticationFailed
                                    | SkPayloadError::InvalidPacket
                                    | SkPayloadError::Ike(_),
                                ) => continue,
                                Err(error) => return Err(error.into()),
                            };
                            match self.tracker.accept_response(&header)? {
                                ResponseDisposition::Duplicate => continue,
                                ResponseDisposition::Accepted => {
                                    return Ok(IkeAuthResponse { header, frames });
                                }
                            }
                        }
                        // IKE and ESP share the NAT-T UDP channel. ESP can be
                        // reordered ahead of the authenticated IKE_AUTH reply
                        // or arrive late from a prior CHILD_SA. The current
                        // exchange cannot authenticate or consume it, so keep
                        // the existing bounded attempt deadline and drop it.
                        EpdgPacket::Keepalive | EpdgPacket::Esp(_) => continue,
                    }
                }
            })
            .await;
            match response {
                Err(_) => continue,
                Ok(Err(error)) => {
                    self.tracker.fail_and_close();
                    return Err(error);
                }
                Ok(Ok(response)) => {
                    // Only inspect notifications after SK authentication. A
                    // peer error must not be hidden by a later missing AUTH/SA.
                    for notify in response.frames.iter().filter(|frame| frame.kind == 41) {
                        if notify.body.len() < 4
                            || notify.body.len() < 4 + usize::from(notify.body[1])
                        {
                            self.tracker.fail_and_close();
                            return Err(IkeAuthError::MalformedNotify);
                        }
                        let code = u16::from_be_bytes([notify.body[2], notify.body[3]]);
                        if code < 16384 {
                            self.tracker.fail_and_close();
                            return Err(IkeAuthError::PeerNotify(code));
                        }
                    }
                    let mut ids = response.frames.iter().filter(|frame| frame.kind == 36);
                    if let Some(id) = ids.next() {
                        if ids.next().is_some()
                            || id.body.len() < 5
                            || self
                                .responder_identity
                                .as_ref()
                                .is_some_and(|old| old.as_slice() != id.body.as_slice())
                        {
                            self.tracker.fail_and_close();
                            return Err(IkeAuthError::ResponderAuthenticationFailed);
                        }
                        self.responder_identity = Some(zeroize::Zeroizing::new(id.body.clone()));
                    }
                    return Ok(response);
                }
            }
        }
        self.tracker.fail_and_close();
        Err(IkeAuthError::Timeout)
    }

    /// Final EAP authentication does not install a CHILD SA or publish readiness.
    /// Consuming self ensures failure/cancellation drops the half-built session.
    pub async fn finish_eap_authentication(
        self,
        transport: &EpdgDatagramTransport,
        eap: crate::EapHandshakeResult,
    ) -> Result<(Self, PendingChildSa), IkeAuthError> {
        self.finish_eap_authentication_with_config(transport, eap, &[])
            .await
    }
    pub async fn finish_eap_authentication_with_config(
        mut self,
        transport: &EpdgDatagramTransport,
        eap: crate::EapHandshakeResult,
        config_frames: &[PayloadFrame],
    ) -> Result<(Self, PendingChildSa), IkeAuthError> {
        if self
            .peer_certificate_policy
            .as_ref()
            .is_some_and(crate::PeerCertificatePolicy::requires_certificate)
            && !self.initial_certificate_verified
        {
            return Err(IkeAuthError::ResponderAuthenticationFailed);
        }
        if config_frames.iter().any(|v| v.kind != 47) {
            return Err(IkeAuthError::UnexpectedPacket);
        }
        self.configuration_requested |= !config_frames.is_empty();
        let auth = self.build_final_eap_auth(&eap.msk)?;
        let mut final_frames = vec![auth];
        final_frames.extend_from_slice(config_frames);
        let response = self
            .exchange(transport, &final_frames, 3, Duration::from_secs(5))
            .await?;
        if response.frames.iter().any(|f| f.kind == 37) {
            return Err(IkeAuthError::ResponderAuthenticationFailed);
        }
        self.verify_responder_eap_auth(&eap.msk, &response.frames)?;
        let offered = self.child_sa_offer.as_ref().ok_or(IkeAuthError::ChildSa(
            crate::ChildSaError::UnofferedProposal,
        ))?;
        let mut choices = response.frames.iter().filter(|frame| frame.kind == 33);
        let selected = choices.next().ok_or(IkeAuthError::ChildSa(
            crate::ChildSaError::UnofferedProposal,
        ))?;
        if choices.next().is_some() {
            return Err(IkeAuthError::ChildSa(
                crate::ChildSaError::UnofferedProposal,
            ));
        }
        let selection = crate::validate_initial_child_sa(offered, &selected.body)
            .map_err(IkeAuthError::ChildSa)?;
        let keys = proletarius_ike::derive_initial_child_sa_keys(
            self.prf_id,
            &self.keys.sk_d,
            &self.initiator_nonce,
            &self.responder_nonce,
            selection.encryption_key_len,
            selection.integrity_key_len,
        )
        .map_err(IkeAuthError::KeyDerivation)?;
        let traffic_selectors =
            crate::validate_child_traffic_selectors(&self.initial_selectors, &response.frames)
                .map_err(IkeAuthError::TrafficSelectors)?;
        let configuration = crate::parse_tunnel_configuration(
            &response.frames,
            self.configuration_requested,
            &traffic_selectors.initiator,
        )
        .map_err(IkeAuthError::Configuration)?;
        self.auth_lifetime = crate::ike_rekey::auth_lifetime(&response.frames)?
            .map(crate::ike_rekey::AuthenticationLifetime::received);
        self.final_auth_verified = true;
        self.eap_direct_material = eap.eap_direct_material;
        // IKE authentication is complete; this is not IMS/data-plane readiness.
        self.child_selection = Some(selection.clone());
        self.child_selectors = response
            .frames
            .iter()
            .filter(|v| matches!(v.kind, 44 | 45))
            .cloned()
            .collect();
        self.tracker.complete_authentication()?;
        Ok((
            self,
            PendingChildSa {
                configuration,
                traffic_selectors,
                response,
                selection,
                keys,
            },
        ))
    }

    fn verify_responder_eap_auth(
        &self,
        msk: &[u8],
        frames: &[PayloadFrame],
    ) -> Result<(), IkeAuthError> {
        let failure = || IkeAuthError::ResponderAuthenticationFailed;
        let identity = self.responder_identity.as_ref().ok_or_else(failure)?;
        let mut auths = frames.iter().filter(|frame| frame.kind == 39);
        let auth = auths.next().ok_or_else(failure)?;
        if auths.next().is_some() || auth.body.len() < 5 || auth.body[0] != 2 {
            return Err(failure());
        }
        let expected = proletarius_ike::compute_eap_auth(
            self.prf_id,
            msk,
            &self.sa_init_response,
            &self.initiator_nonce,
            &self.keys.sk_pr,
            identity,
        )
        .map_err(IkeAuthError::KeyDerivation)?;
        if !bool::from(expected.as_slice().ct_eq(&auth.body[4..])) {
            return Err(failure());
        }
        Ok(())
    }

    /// Final initiator AUTH uses only the recorded SA_INIT request and the IDi
    /// actually encrypted in the first IKE_AUTH round, including COOKIE retry.
    pub fn build_final_eap_auth(&self, msk: &[u8]) -> Result<PayloadFrame, IkeAuthError> {
        let identity = self
            .initiator_identity
            .as_deref()
            .ok_or(IkeAuthError::UnexpectedPacket)?;
        let auth = proletarius_ike::compute_eap_auth(
            self.prf_id,
            msk,
            &self.sa_init_request,
            &self.responder_nonce,
            &self.keys.sk_pi,
            identity,
        )
        .map_err(IkeAuthError::KeyDerivation)?;
        let mut body = vec![2, 0, 0, 0];
        body.extend_from_slice(&auth);
        Ok(PayloadFrame {
            kind: 39,
            critical: false,
            body,
        })
    }
}
