use proletarius_qmi_services::ImsSettingsUpdate;
use proletarius_qmi_services::{
    CodecError, IMS_GET_SERVICES, IMSA_GET_REGISTRATION, ImsSettings, ImsaRegistrationStatus,
    NasOperatorName, SERVICE_IMS, decode_ims_services_response, decode_ims_settings_indication,
    decode_imsa_registration_indication, decode_imsa_registration_response,
    encode_ims_get_services_request, encode_ims_set_services_request,
};
use proletarius_qmi_wire::{PacketMeta, Tlv, encode_packet};

#[test]
fn nas_operator_name_prefers_spn_then_falls_back_to_operator_string() {
    let name = NasOperatorName {
        service_provider_name: "Carrier SPN".into(),
        operator_string_name: "Carrier Name".into(),
    };
    assert_eq!(name.preferred_name(), "Carrier SPN");

    let name = NasOperatorName {
        service_provider_name: String::new(),
        operator_string_name: "Carrier Name".into(),
    };
    assert_eq!(name.preferred_name(), "Carrier Name");
}

#[test]
fn ims_get_request_matches_go_qmi_qmux_message() {
    let mut output = [0_u8; 32];
    let length = encode_ims_get_services_request(7, 0x1234, &mut output).unwrap();
    assert_eq!(
        &output[..length],
        [
            0x01, 0x0c, 0x00, 0x00, 0x12, 0x07, 0x00, 0x34, 0x12, 0x90, 0x00, 0x00, 0x00
        ]
    );
}

#[test]
fn ims_set_request_preserves_go_field_order_and_explicit_false() {
    let mut output = [0_u8; 64];
    let length = encode_ims_set_services_request(
        4,
        9,
        ImsSettingsUpdate {
            voice_over_lte: Some(false),
            call_mode: Some(3),
            presence: Some(true),
            ..ImsSettings::default()
        },
        &mut output,
    )
    .unwrap();
    let expected = [
        0x01, 0x18, 0x00, 0x00, 0x12, 0x04, 0x00, 0x09, 0x00, 0x8f, 0x00, 0x0c, 0x00, 0x10, 0x01,
        0x00, 0x00, 0x15, 0x01, 0x00, 0x03, 0x1e, 0x01, 0x00, 0x01,
    ];
    assert_eq!(&output[..length], expected);
}

#[test]
fn ims_get_response_matches_go_supported_fields_and_ignores_unexposed_fields() {
    let mut bytes = [0_u8; 64];
    let tlvs = [
        Tlv::new(0x02, &[0, 0, 0, 0]),
        Tlv::new(0x11, &[1]),
        Tlv::new(0x12, &[0]),
        Tlv::new(0x15, &[1]),
        Tlv::new(0x16, &[3]), // Celmux parses this; Go does not surface it.
        Tlv::new(0x18, &[1]),
        Tlv::new(0x19, &[0]),
        Tlv::new(0x1a, &[1]),
        Tlv::new(0x1c, &[1]),
    ];
    let meta = response_meta(SERVICE_IMS, IMS_GET_SERVICES);
    let length = encode_packet(meta, &tlvs, &mut bytes).unwrap();
    assert_eq!(
        decode_ims_services_response(&bytes[..length]).unwrap(),
        ImsSettings {
            voice_over_lte: Some(true),
            video_telephony: Some(false),
            voice_wifi: Some(true),
            ims: Some(true),
            ut: Some(false),
            sms: Some(true),
            ussd: Some(true),
            ..ImsSettings::default()
        }
    );
}

#[test]
fn imsa_response_reads_registration_state_and_optional_fields_like_go() {
    let mut bytes = [0_u8; 64];
    let tlvs = [
        Tlv::new(0x02, &[0, 0, 0, 0]),
        Tlv::new(0x11, &[0x34, 0x12]),
        Tlv::new(0x12, &[2, 0, 0, 0]),
        Tlv::new(0x13, b"registered"),
        Tlv::new(0x14, &[0, 0, 0, 0]),
    ];
    let length = encode_packet(
        response_meta(0x21, IMSA_GET_REGISTRATION),
        &tlvs,
        &mut bytes,
    )
    .unwrap();
    assert_eq!(
        decode_imsa_registration_response(&bytes[..length]).unwrap(),
        ImsaRegistrationStatus {
            state: 2,
            error_code: Some(0x1234),
            error_message: Some("registered".into()),
            technology: Some(0),
        }
    );
}

#[test]
fn indication_parsers_use_go_tlv_maps_and_require_indication_flags() {
    let mut bytes = [0_u8; 64];
    let ims_tlvs = [Tlv::new(0x14, &[0]), Tlv::new(0x1e, &[1])];
    let mut ims_meta = PacketMeta::request(SERVICE_IMS, 2, 3, 0x0091);
    ims_meta.message_control_flags = 4;
    let length = encode_packet(ims_meta, &ims_tlvs, &mut bytes).unwrap();
    assert_eq!(
        decode_ims_settings_indication(&bytes[..length]).unwrap(),
        ImsSettings {
            voice_wifi: Some(false),
            presence: Some(true),
            ..ImsSettings::default()
        }
    );

    let imsa_tlvs = [
        Tlv::new(0x10, &[0x34, 0x12]),
        Tlv::new(0x11, &[3, 0, 0, 0]),
        Tlv::new(0x12, b"limited"),
        Tlv::new(0x13, &[2, 0, 0, 0]),
    ];
    let mut imsa_meta = PacketMeta::request(0x21, 2, 4, 0x0023);
    imsa_meta.message_control_flags = 4;
    let length = encode_packet(imsa_meta, &imsa_tlvs, &mut bytes).unwrap();
    assert_eq!(
        decode_imsa_registration_indication(&bytes[..length]).unwrap(),
        ImsaRegistrationStatus {
            state: 3,
            error_code: Some(0x1234),
            error_message: Some("limited".into()),
            technology: Some(2),
        }
    );
}

#[test]
fn response_parser_rejects_failed_qmi_result_and_wrong_message() {
    let mut bytes = [0_u8; 32];
    let tlvs = [Tlv::new(0x02, &[1, 0, 0x47, 0])];
    let length = encode_packet(
        response_meta(SERVICE_IMS, IMS_GET_SERVICES),
        &tlvs,
        &mut bytes,
    )
    .unwrap();
    assert_eq!(
        decode_ims_services_response(&bytes[..length]),
        Err(CodecError::QmiFailure {
            result: 1,
            error: 0x47
        })
    );

    let length = encode_packet(
        response_meta(SERVICE_IMS, IMS_GET_SERVICES + 1),
        &tlvs,
        &mut bytes,
    )
    .unwrap();
    assert!(matches!(
        decode_ims_services_response(&bytes[..length]),
        Err(CodecError::UnexpectedMessage { .. })
    ));
}

#[test]
fn response_parser_rejects_indications_without_a_response_flag() {
    let mut bytes = [0_u8; 32];
    let tlvs = [Tlv::new(0x02, &[0, 0, 0, 0])];
    let mut meta = PacketMeta::request(SERVICE_IMS, 7, 1, IMS_GET_SERVICES);
    meta.message_control_flags = 4;
    let length = encode_packet(meta, &tlvs, &mut bytes).unwrap();
    assert_eq!(
        decode_ims_services_response(&bytes[..length]),
        Err(CodecError::MalformedField("QMI response control flag"))
    );
}

fn response_meta(service: u16, message_id: u16) -> PacketMeta {
    let mut meta = PacketMeta::request(service, 7, 1, message_id);
    meta.message_control_flags = 2;
    meta
}
