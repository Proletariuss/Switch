use proletarius_ccid::{Error, apdu, bertlv};

#[test]
fn definite_constructed_tlv_round_trips_and_preserves_nested_content() {
    let mut encoded = Vec::new();
    bertlv::encode_constructed(&[0xbf, 0x2d], &[0x80, 0x01, 0x00], &mut encoded).unwrap();

    let mut items = bertlv::parse(&encoded).unwrap();
    let outer = items.next().unwrap().unwrap();
    assert!(items.next().is_none());
    assert_eq!(outer.tag.bytes, [0xbf, 0x2d]);
    let child = outer.children().unwrap().next().unwrap().unwrap();
    assert_eq!(child.tag.bytes, [0x80]);
    assert_eq!(child.value, [0x00]);
}

#[test]
fn ber_parser_rejects_indefinite_and_truncated_lengths() {
    assert!(matches!(
        bertlv::parse(&[0x5a, 0x80]).unwrap().next().unwrap(),
        Err(Error::Unsupported("indefinite BER length"))
    ));
    assert!(matches!(
        bertlv::parse(&[0x5a, 0x02, 0x01]).unwrap().next().unwrap(),
        Err(Error::Truncated)
    ));
}

#[test]
fn response_parser_keeps_status_word_separate_from_payload() {
    let response = apdu::Response::parse(&[0x01, 0x02, 0x90, 0x00]).unwrap();
    assert_eq!(response.data, [0x01, 0x02]);
    assert_eq!(response.status, 0x9000);
    assert!(response.ok());
}
