use proletarius_qmi_wire::{PacketMeta, Tlv, decode_packet, encode_packet};

#[test]
fn quectel_go_service_packet_matches_qmux_golden_bytes() {
    let meta = PacketMeta::request(3, 7, 0x1234, 0x00ad);
    let tlvs = [Tlv::new(0x10, &[0xaa, 0xbb])];
    let mut encoded = [0_u8; 32];

    let length = encode_packet(meta, &tlvs, &mut encoded).expect("encode QMUX request");
    let expected = [
        0x01, 0x11, 0x00, 0x00, 0x03, 0x07, // QMUX header
        0x00, 0x34, 0x12, 0xad, 0x00, 0x05, 0x00, // QMI service header
        0x10, 0x02, 0x00, 0xaa, 0xbb, // TLV
    ];
    assert_eq!(&encoded[..length], expected);

    let decoded = decode_packet(&expected).expect("decode QMUX golden packet");
    assert_eq!(decoded.meta, meta);
    assert_eq!(decoded.tlvs().collect::<Vec<_>>(), tlvs);
}

#[test]
fn control_service_uses_one_byte_transaction_id_like_go_qmi() {
    let meta = PacketMeta::request(0, 1, 0x1234, 0x0022);
    let tlvs = [Tlv::new(0x01, &[0x02])];
    let mut encoded = [0_u8; 32];

    let length = encode_packet(meta, &tlvs, &mut encoded).expect("encode QMI CTL request");
    let expected = [
        0x01, 0x0f, 0x00, 0x00, 0x00, 0x01, // QMUX header
        0x00, 0x34, 0x22, 0x00, 0x04, 0x00, // CTL header (transaction ID truncates to u8)
        0x01, 0x01, 0x00, 0x02, // TLV
    ];
    assert_eq!(&encoded[..length], expected);

    let decoded = decode_packet(&expected).expect("decode QMI CTL golden packet");
    assert_eq!(decoded.meta.transaction_id, 0x34);
    assert_eq!(decoded.meta.message_id, 0x22);
    assert_eq!(decoded.tlvs().collect::<Vec<_>>(), tlvs);
}

#[test]
fn extended_qrtr_service_round_trips_without_changing_qmux_ids() {
    let meta = PacketMeta::request(0x0102, 9, 0x4567, 0x89ab);
    let tlvs = [Tlv::new(0x7f, &[1, 2, 3])];
    let mut encoded = [0_u8; 32];

    let length = encode_packet(meta, &tlvs, &mut encoded).expect("encode extended QRTR frame");
    assert_eq!(encoded[0], 0x02);
    let decoded = decode_packet(&encoded[..length]).expect("decode extended QRTR frame");
    assert_eq!(decoded.meta, meta);
    assert_eq!(decoded.tlvs().collect::<Vec<_>>(), tlvs);
}
