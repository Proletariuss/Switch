// Copyright (c) 2026 iniwex5
// Modifications copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT
//
// Adapted from Celmux crates/qmi/services/{ims,nas,uim}.rs (MIT). The public data
// shape and response-field selection intentionally preserve Switch's current
// quectel-qmi-go behavior where it differs from Celmux.

//! Typed QMI IMS/IMSA codecs, independent of the device transport.
//!
//! It also exposes a client wrapper over the shared QMI transaction core.
//! USB discovery and device lifetime remain with the platform owner.

pub mod client;
mod uim;
pub use client::{ImsClient, ImsClientError, NasClient, NasClientError, QmiServiceClientError};
pub use uim::{NativeMccMnc, UimClient, UimClientError};

use core::fmt;
use proletarius_qmi_wire::{
    DecodeError, DecodedPacket, EncodeError, PacketMeta, Tlv, decode_packet, encode_packet,
};

pub const SERVICE_IMS: u16 = 0x12;
pub const SERVICE_IMSA: u16 = 0x21;
pub const SERVICE_NAS: u16 = 0x03;
pub const SERVICE_UIM: u16 = 0x0b;

pub const IMS_SET_SERVICES: u16 = 0x008f;
pub const IMS_GET_SERVICES: u16 = 0x0090;
pub const IMS_SETTINGS_CHANGED: u16 = 0x0091;
pub const IMSA_GET_REGISTRATION: u16 = 0x0020;
pub const IMSA_REGISTRATION_CHANGED: u16 = 0x0023;
pub const NAS_GET_OPERATOR_NAME: u16 = 0x0039;
pub const UIM_READ_TRANSPARENT: u16 = 0x0020;
pub const UIM_READ_RECORD: u16 = 0x0021;
pub const UIM_GET_FILE_ATTRIBUTES: u16 = 0x0024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NasOperatorName {
    pub service_provider_name: String,
    pub operator_string_name: String,
}

impl NasOperatorName {
    /// Keeps the operator-name preference used by the Go QMI enhancement.
    #[must_use]
    pub fn preferred_name(&self) -> &str {
        if self.service_provider_name.is_empty() {
            &self.operator_string_name
        } else {
            &self.service_provider_name
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImsSettings {
    pub voice_over_lte: Option<bool>,
    pub video_telephony: Option<bool>,
    pub voice_wifi: Option<bool>,
    pub call_mode: Option<u8>,
    pub ims: Option<bool>,
    pub ut: Option<bool>,
    pub sms: Option<bool>,
    pub ussd: Option<bool>,
    pub presence: Option<bool>,
    pub autoconfig: Option<bool>,
    pub xdm_client: Option<bool>,
    pub rcs: Option<bool>,
    pub carrier_config: Option<bool>,
}

pub type ImsSettingsUpdate = ImsSettings;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImsaRegistrationStatus {
    pub state: u32,
    pub error_code: Option<u16>,
    pub error_message: Option<String>,
    pub technology: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodecError {
    Decode(DecodeError),
    Encode(EncodeError),
    UnexpectedService { expected: u16, actual: u16 },
    UnexpectedMessage { expected: u16, actual: u16 },
    MissingResult,
    QmiFailure { result: u16, error: u16 },
    MissingField(&'static str),
    MalformedField(&'static str),
    EmptyUpdate,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => write!(f, "{error}"),
            Self::Encode(error) => write!(f, "{error}"),
            Self::UnexpectedService { expected, actual } => {
                write!(
                    f,
                    "unexpected QMI service 0x{actual:02x}; expected 0x{expected:02x}"
                )
            }
            Self::UnexpectedMessage { expected, actual } => {
                write!(
                    f,
                    "unexpected QMI message 0x{actual:04x}; expected 0x{expected:04x}"
                )
            }
            Self::MissingResult => f.write_str("QMI response is missing its result TLV"),
            Self::QmiFailure { result, error } => {
                write!(
                    f,
                    "QMI request failed (result={result}, error=0x{error:04x})"
                )
            }
            Self::MissingField(field) => write!(f, "QMI response is missing {field}"),
            Self::MalformedField(field) => write!(f, "QMI response has malformed {field}"),
            Self::EmptyUpdate => f.write_str("IMS settings update contains no fields"),
        }
    }
}

impl std::error::Error for CodecError {}

impl From<DecodeError> for CodecError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl From<EncodeError> for CodecError {
    fn from(value: EncodeError) -> Self {
        Self::Encode(value)
    }
}

/// Encodes the Go-compatible IMS get-services request (message 0x0090).
pub fn encode_ims_get_services_request(
    client_id: u8,
    transaction_id: u16,
    output: &mut [u8],
) -> Result<usize, CodecError> {
    encode_empty_request(
        SERVICE_IMS,
        client_id,
        transaction_id,
        IMS_GET_SERVICES,
        output,
    )
}

/// Encodes an IMS set-services request while preserving explicit `false` values.
pub fn encode_ims_set_services_request(
    client_id: u8,
    transaction_id: u16,
    update: ImsSettingsUpdate,
    output: &mut [u8],
) -> Result<usize, CodecError> {
    let mut fields = Vec::with_capacity(13);
    push_bool(&mut fields, 0x10, update.voice_over_lte);
    push_bool(&mut fields, 0x11, update.video_telephony);
    push_bool(&mut fields, 0x14, update.voice_wifi);
    if let Some(value) = update.call_mode {
        fields.push((0x15, value));
    }
    push_bool(&mut fields, 0x18, update.ims);
    push_bool(&mut fields, 0x19, update.ut);
    push_bool(&mut fields, 0x1a, update.sms);
    push_bool(&mut fields, 0x1c, update.ussd);
    push_bool(&mut fields, 0x1e, update.presence);
    push_bool(&mut fields, 0x1f, update.autoconfig);
    push_bool(&mut fields, 0x20, update.xdm_client);
    push_bool(&mut fields, 0x21, update.rcs);
    push_bool(&mut fields, 0x25, update.carrier_config);
    if fields.is_empty() {
        return Err(CodecError::EmptyUpdate);
    }
    let tlvs = fields
        .iter()
        .map(|(kind, value)| Tlv::new(*kind, core::slice::from_ref(value)))
        .collect::<Vec<_>>();
    encode_qmi_request(
        PacketMeta::request(SERVICE_IMS, client_id, transaction_id, IMS_SET_SERVICES),
        &tlvs,
        output,
    )
}

/// Encodes an IMSA registration-status query (message 0x0020).
pub fn encode_imsa_registration_request(
    client_id: u8,
    transaction_id: u16,
    output: &mut [u8],
) -> Result<usize, CodecError> {
    encode_empty_request(
        SERVICE_IMSA,
        client_id,
        transaction_id,
        IMSA_GET_REGISTRATION,
        output,
    )
}

/// Decodes the IMS settings response fields returned by Switch's Go backend.
/// As in that backend, settings not currently surfaced by the response parser
/// (including call mode and presence settings) remain `None`.
pub fn decode_ims_services_response(frame: &[u8]) -> Result<ImsSettings, CodecError> {
    let packet = checked_response(frame, SERVICE_IMS, IMS_GET_SERVICES)?;
    Ok(ImsSettings {
        voice_over_lte: bool_field(&packet, 0x11),
        video_telephony: bool_field(&packet, 0x12),
        voice_wifi: bool_field(&packet, 0x15),
        ims: bool_field(&packet, 0x18),
        ut: bool_field(&packet, 0x19),
        sms: bool_field(&packet, 0x1a),
        ussd: bool_field(&packet, 0x1c),
        ..ImsSettings::default()
    })
}

/// Decodes the IMSA registration response with the same required state TLV as Go.
pub fn decode_imsa_registration_response(
    frame: &[u8],
) -> Result<ImsaRegistrationStatus, CodecError> {
    let packet = checked_response(frame, SERVICE_IMSA, IMSA_GET_REGISTRATION)?;
    parse_imsa_registration(&packet, true)
}

/// Decodes an IMS settings-changed indication using Go's indication TLV map.
pub fn decode_ims_settings_indication(frame: &[u8]) -> Result<ImsSettings, CodecError> {
    let packet = decode_packet(frame)?;
    check_meta(&packet, SERVICE_IMS, IMS_SETTINGS_CHANGED)?;
    if !packet.is_indication() {
        return Err(CodecError::MalformedField("IMS indication control flag"));
    }
    Ok(ImsSettings {
        voice_over_lte: bool_field(&packet, 0x10),
        video_telephony: bool_field(&packet, 0x11),
        voice_wifi: bool_field(&packet, 0x14),
        ims: bool_field(&packet, 0x18),
        ut: bool_field(&packet, 0x19),
        sms: bool_field(&packet, 0x1a),
        ussd: bool_field(&packet, 0x1c),
        presence: bool_field(&packet, 0x1e),
        autoconfig: bool_field(&packet, 0x1f),
        xdm_client: bool_field(&packet, 0x20),
        rcs: bool_field(&packet, 0x21),
        carrier_config: bool_field(&packet, 0x25),
        ..ImsSettings::default()
    })
}

/// Decodes IMSA registration-status indications.
pub fn decode_imsa_registration_indication(
    frame: &[u8],
) -> Result<ImsaRegistrationStatus, CodecError> {
    let packet = decode_packet(frame)?;
    check_meta(&packet, SERVICE_IMSA, IMSA_REGISTRATION_CHANGED)?;
    if !packet.is_indication() {
        return Err(CodecError::MalformedField("IMSA indication control flag"));
    }
    parse_imsa_registration(&packet, false)
}

fn encode_empty_request(
    service: u16,
    client_id: u8,
    transaction_id: u16,
    message_id: u16,
    output: &mut [u8],
) -> Result<usize, CodecError> {
    encode_qmi_request(
        PacketMeta::request(service, client_id, transaction_id, message_id),
        &[],
        output,
    )
}

fn encode_qmi_request(
    meta: PacketMeta,
    tlvs: &[Tlv<'_>],
    output: &mut [u8],
) -> Result<usize, CodecError> {
    encode_packet(meta, tlvs, output).map_err(CodecError::from)
}

fn push_bool(fields: &mut Vec<(u8, u8)>, kind: u8, value: Option<bool>) {
    if let Some(value) = value {
        fields.push((kind, u8::from(value)));
    }
}

fn checked_response(
    frame: &[u8],
    service: u16,
    message: u16,
) -> Result<DecodedPacket<'_>, CodecError> {
    let packet = decode_packet(frame)?;
    check_meta(&packet, service, message)?;
    let result = packet.result_code().ok_or(CodecError::MissingResult)?;
    if result.result != 0 {
        return Err(CodecError::QmiFailure {
            result: result.result,
            error: result.error,
        });
    }
    if packet.meta.message_control_flags & 0x02 == 0 || packet.is_indication() {
        return Err(CodecError::MalformedField("QMI response control flag"));
    }
    Ok(packet)
}

fn check_meta(packet: &DecodedPacket<'_>, service: u16, message: u16) -> Result<(), CodecError> {
    if packet.meta.service != service {
        return Err(CodecError::UnexpectedService {
            expected: service,
            actual: packet.meta.service,
        });
    }
    if packet.meta.message_id != message {
        return Err(CodecError::UnexpectedMessage {
            expected: message,
            actual: packet.meta.message_id,
        });
    }
    Ok(())
}

fn bool_field(packet: &DecodedPacket<'_>, kind: u8) -> Option<bool> {
    packet
        .find_tlv(kind)?
        .value
        .first()
        .map(|value| *value != 0)
}

fn parse_imsa_registration(
    packet: &DecodedPacket<'_>,
    response: bool,
) -> Result<ImsaRegistrationStatus, CodecError> {
    let (state_kind, error_kind, technology_kind) = if response {
        (0x12, 0x11, 0x14)
    } else {
        (0x11, 0x10, 0x13)
    };
    let message_kind = if response { 0x13 } else { 0x12 };
    let state = packet
        .find_tlv(state_kind)
        .ok_or(CodecError::MissingField("IMSA registration state"))?;
    if state.value.len() < 4 {
        return Err(CodecError::MalformedField("IMSA registration state"));
    }
    let error_code = packet
        .find_tlv(error_kind)
        .map(|tlv| {
            if tlv.value.len() < 2 {
                return Err(CodecError::MalformedField("IMSA error code"));
            }
            Ok(u16::from_le_bytes([tlv.value[0], tlv.value[1]]))
        })
        .transpose()?;
    let technology = packet
        .find_tlv(technology_kind)
        .map(|tlv| {
            if tlv.value.len() < 4 {
                return Err(CodecError::MalformedField("IMSA registration technology"));
            }
            Ok(u32::from_le_bytes([
                tlv.value[0],
                tlv.value[1],
                tlv.value[2],
                tlv.value[3],
            ]))
        })
        .transpose()?;
    let error_message = packet
        .find_tlv(message_kind)
        .map(|tlv| String::from_utf8_lossy(tlv.value).into_owned());
    Ok(ImsaRegistrationStatus {
        state: u32::from_le_bytes([
            state.value[0],
            state.value[1],
            state.value[2],
            state.value[3],
        ]),
        error_code,
        error_message,
        technology,
    })
}
