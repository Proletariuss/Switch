use super::*;
use crate::ExchangeType;
fn packet(frames: &[PayloadFrame], response: bool) -> Vec<u8> {
    let (first, body) = crate::encode_payload_chain(frames).unwrap();
    IkeHeader {
        initiator_spi: 1,
        responder_spi: if response { 2 } else { 0 },
        next_payload: first,
        version: 0x20,
        exchange: ExchangeType::IkeSaInit,
        flags: if response {
            IkeHeader::RESPONSE_FLAG
        } else {
            IkeHeader::INITIATOR_FLAG
        },
        message_id: 0,
    }
    .encode(&body)
    .unwrap()
}
fn auth(method: u8, data: &[u8]) -> PayloadFrame {
    PayloadFrame {
        kind: 39,
        critical: false,
        body: [&[method, 7, 8, 9][..], data].concat(),
    }
}
#[test]
fn signed_octets_keep_complete_transcript_nonce_and_exact_idr() {
    let actual =
        crate::auth_signed_octets(5, &[6; 28], &[7; 16], b"sk_pr", &[2, 1, 2, 3, b'x']).unwrap();
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"sk_pr").unwrap();
    mac.update(&[2, 1, 2, 3, b'x']);
    let hashed = mac.finalize().into_bytes();
    assert_eq!(
        actual.as_slice(),
        [&[6; 28][..], &[7; 16][..], &hashed].concat()
    );
    assert_ne!(
        actual,
        crate::auth_signed_octets(5, &[6; 28], &[7; 16], b"sk_pr", &[2, 0, 0, 0, b'x']).unwrap()
    );
}
#[test]
fn certificate_chain_rejects_wrong_encoding_duplicates_der_and_limits() {
    let cert = PayloadFrame {
        kind: 37,
        critical: false,
        body: vec![4, 0x30, 1, 0],
    };
    assert_eq!(
        certificate_chain(std::slice::from_ref(&cert)).unwrap(),
        vec![&cert.body[1..]]
    );
    assert_eq!(
        certificate_chain(&[cert.clone(), cert.clone()]).unwrap_err(),
        CertificateAuthError::DuplicateCertificate
    );
    for body in [
        vec![12, 0x30, 1, 0],
        vec![4, 0x30, 0x80, 1, 0],
        vec![4, 0x30, 0x81, 1, 0],
        vec![4, 0x30, 1, 0, 0],
    ] {
        assert_eq!(
            certificate_chain(&[PayloadFrame {
                body,
                ..cert.clone()
            }])
            .unwrap_err(),
            CertificateAuthError::InvalidCertificate
        );
    }
    assert_eq!(
        certificate_chain(&[PayloadFrame {
            body: vec![4; MAX_CERTIFICATE_BYTES + 2],
            ..cert.clone()
        }])
        .unwrap_err(),
        CertificateAuthError::ChainTooLarge
    );
    let chain: Vec<_> = (0..9)
        .map(|i| PayloadFrame {
            body: vec![4, 0x30, 1, i],
            ..cert.clone()
        })
        .collect();
    assert_eq!(
        certificate_chain(&chain).unwrap_err(),
        CertificateAuthError::ChainTooLarge
    );
}
#[test]
fn digital_signature_requires_actual_notify_and_exact_der_algorithm() {
    let alg = [
        0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 11, 5, 0,
    ];
    let data = [&[alg.len() as u8][..], &alg, &[1, 2, 3]].concat();
    let frames = [auth(14, &data)];
    let request = packet(&[signature_hash_offer()], false);
    let response = packet(&[signature_hash_offer()], true);
    assert_eq!(
        certificate_signature(&frames, &request, &response)
            .unwrap()
            .algorithm,
        SignatureAlgorithm::RsaPkcs1Sha256
    );
    assert!(matches!(
        certificate_signature(&frames, &request, &packet(&[], true)),
        Err(CertificateAuthError::UnnegotiatedAlgorithm)
    ));
    let mut malformed = data.clone();
    malformed[2] = 0x80;
    assert!(matches!(
        certificate_signature(&[auth(14, &malformed)], &request, &response),
        Err(CertificateAuthError::InvalidAlgorithm)
    ));
    let mut pss = data.clone();
    pss[13] = 10;
    assert!(matches!(
        certificate_signature(&[auth(14, &pss)], &request, &response),
        Err(CertificateAuthError::UnsupportedAlgorithm)
    ));
    assert!(matches!(
        certificate_signature(&[frames[0].clone(), frames[0].clone()], &request, &response),
        Err(CertificateAuthError::DuplicateAuthentication)
    ));
    assert!(matches!(
        certificate_signature(&[], &request, &response),
        Err(CertificateAuthError::MissingAuthentication)
    ));
    assert_eq!(
        certificate_signature(&[auth(1, &[1, 2])], &[], &[])
            .unwrap()
            .algorithm,
        SignatureAlgorithm::RsaPkcs1Sha1
    );
}
