use std::net::SocketAddr;

use proletarius_ike::{
    NatDetectionKind, decode_payload_chain, encode_payload_chain, nat_detection_notify,
};

#[test]
fn nat_detection_ipv4_wire_matches_independent_sha1_vectors() {
    let source: SocketAddr = "192.0.2.10:500".parse().unwrap();
    let destination: SocketAddr = "198.51.100.20:500".parse().unwrap();
    let source = nat_detection_notify(NatDetectionKind::Source, 0x0102_0304_0506_0708, 0, source);
    let destination = nat_detection_notify(
        NatDetectionKind::Destination,
        0x0102_0304_0506_0708,
        0,
        destination,
    );
    assert_eq!(source.kind, 41);
    assert!(!source.critical);
    assert_eq!(
        source.body,
        [
            1, 0, 0x40, 0x04, 0x9c, 0xda, 0xf8, 0x71, 0x3c, 0xea, 0x81, 0xe2, 0x87, 0xd3, 0xc0,
            0xba, 0xcb, 0x7d, 0xb4, 0x68, 0x69, 0x1d, 0xfe, 0x58,
        ]
    );
    assert_eq!(
        destination.body,
        [
            1, 0, 0x40, 0x05, 0xfc, 0xb1, 0x49, 0xf8, 0x3b, 0x2c, 0x81, 0x09, 0x85, 0x8a, 0x29,
            0x16, 0x9d, 0x21, 0x22, 0x28, 0x5d, 0x91, 0x8f, 0xba,
        ]
    );
    let (first, wire) = encode_payload_chain(&[source.clone(), destination.clone()]).unwrap();
    assert_eq!(first, 41);
    assert_eq!(wire[0], 41);
    assert_eq!(wire[1..4], [0, 0, 28]);
    assert_eq!(wire[28..32], [0, 0, 0, 28]);
    assert_eq!(
        decode_payload_chain(first, &wire).unwrap(),
        [source, destination]
    );
}

#[test]
fn nat_detection_ipv6_uses_sixteen_network_order_address_bytes() {
    let endpoint: SocketAddr = "[2001:db8::1234]:500".parse().unwrap();
    let notify = nat_detection_notify(NatDetectionKind::Source, 0x0102_0304_0506_0708, 0, endpoint);
    assert_eq!(
        notify.body,
        [
            1, 0, 0x40, 0x04, 0xdb, 0xc3, 0x5a, 0x6a, 0x97, 0xc1, 0xee, 0x58, 0x63, 0xc9, 0x37,
            0xdd, 0x5b, 0xc9, 0x62, 0xb8, 0xcd, 0x57, 0x46, 0x4c,
        ]
    );
}

#[test]
fn nat_detection_unspecified_source_hashes_reported_zero_address() {
    let source: SocketAddr = "0.0.0.0:500".parse().unwrap();
    let notify = nat_detection_notify(NatDetectionKind::Source, 0x0102_0304_0506_0708, 0, source);
    assert_eq!(
        notify.body,
        [
            1, 0, 0x40, 0x04, 0xaf, 0xae, 0x71, 0xf0, 0xeb, 0xed, 0xef, 0x84, 0x46, 0xe4, 0x48,
            0xa5, 0xf5, 0x04, 0xb6, 0x1e, 0xd9, 0x94, 0x25, 0xb5,
        ]
    );
}
