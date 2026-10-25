use crate::{
    CodecError, IMS_GET_SERVICES, IMS_SET_SERVICES, IMSA_GET_REGISTRATION, ImsSettings,
    ImsSettingsUpdate, ImsaRegistrationStatus, NAS_GET_OPERATOR_NAME, NasOperatorName, SERVICE_IMS,
    SERVICE_IMSA, SERVICE_NAS,
};
use proletarius_qmi_core::{Client, ClientError, OwnedPacket, ServiceClient};
use proletarius_qmi_wire::Tlv;
use std::fmt;

/// Paired IMS and IMSA clients sharing the caller-owned QMI transport.
///
/// Opening allocates distinct QMI client IDs. Dropping or explicitly closing
/// the wrapper releases those IDs while leaving the shared CTL transport to
/// its owner.
#[derive(Clone)]
pub struct ImsClient {
    ims: ServiceClient,
    imsa: ServiceClient,
}

#[derive(Clone)]
pub struct NasClient {
    nas: ServiceClient,
}

#[derive(Debug)]
pub enum QmiServiceClientError {
    Client(ClientError),
    Codec(CodecError),
}

impl fmt::Display for QmiServiceClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => write!(f, "{error}"),
            Self::Codec(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for QmiServiceClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Client(error) => Some(error),
            Self::Codec(error) => Some(error),
        }
    }
}

pub type ImsClientError = QmiServiceClientError;
pub type NasClientError = QmiServiceClientError;

impl From<ClientError> for QmiServiceClientError {
    fn from(error: ClientError) -> Self {
        Self::Client(error)
    }
}

impl From<CodecError> for QmiServiceClientError {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}

impl ImsClient {
    pub fn open(client: &Client) -> Result<Self, ClientError> {
        let ims = ServiceClient::open(client, SERVICE_IMS)?;
        let imsa = ServiceClient::open(client, SERVICE_IMSA)?;
        Ok(Self { ims, imsa })
    }

    pub fn get_services(&self) -> Result<ImsSettings, ImsClientError> {
        let response = self.ims.request(IMS_GET_SERVICES, &[])?;
        validate_response(&response, SERVICE_IMS, IMS_GET_SERVICES)?;
        Ok(ImsSettings {
            voice_over_lte: bool_field(&response, 0x11),
            video_telephony: bool_field(&response, 0x12),
            voice_wifi: bool_field(&response, 0x15),
            ims: bool_field(&response, 0x18),
            ut: bool_field(&response, 0x19),
            sms: bool_field(&response, 0x1a),
            ussd: bool_field(&response, 0x1c),
            ..ImsSettings::default()
        })
    }

    pub fn set_services(&self, update: ImsSettingsUpdate) -> Result<(), ImsClientError> {
        let fields = settings_tlvs(update)?;
        let tlvs = fields
            .iter()
            .map(|(kind, value)| Tlv::new(*kind, std::slice::from_ref(value)))
            .collect::<Vec<_>>();
        let response = self.ims.request(IMS_SET_SERVICES, &tlvs)?;
        validate_response(&response, SERVICE_IMS, IMS_SET_SERVICES)?;
        Ok(())
    }

    pub fn get_registration_status(&self) -> Result<ImsaRegistrationStatus, ImsClientError> {
        let response = self.imsa.request(IMSA_GET_REGISTRATION, &[])?;
        validate_response(&response, SERVICE_IMSA, IMSA_GET_REGISTRATION)?;
        parse_registration(&response)
    }

    /// Releases both service client IDs. Both releases are attempted even if
    /// the first one fails, so a partial modem-side cleanup is observable but
    /// does not silently skip the other service.
    pub fn close(&self) -> Result<(), ClientError> {
        let imsa = self.imsa.close();
        let ims = self.ims.close();
        imsa.and(ims)
    }
}

impl NasClient {
    pub fn open(client: &Client) -> Result<Self, ClientError> {
        Ok(Self {
            nas: ServiceClient::open(client, SERVICE_NAS)?,
        })
    }

    pub fn get_operator_name(&self) -> Result<NasOperatorName, NasClientError> {
        let response = self.nas.request(NAS_GET_OPERATOR_NAME, &[])?;
        validate_response(&response, SERVICE_NAS, NAS_GET_OPERATOR_NAME)?;
        Ok(NasOperatorName {
            service_provider_name: response
                .find_tlv(0x10)
                .filter(|value| !value.is_empty())
                .map(|value| String::from_utf8_lossy(&value[1..]).into_owned())
                .unwrap_or_default(),
            operator_string_name: response
                .find_tlv(0x13)
                .map(|value| String::from_utf8_lossy(value).into_owned())
                .unwrap_or_default(),
        })
    }

    pub fn close(&self) -> Result<(), ClientError> {
        self.nas.close()
    }
}

pub(crate) fn validate_response(
    response: &OwnedPacket,
    service: u16,
    message: u16,
) -> Result<(), QmiServiceClientError> {
    if response.meta.service != service {
        return Err(CodecError::UnexpectedService {
            expected: service,
            actual: response.meta.service,
        }
        .into());
    }
    if response.meta.message_id != message {
        return Err(CodecError::UnexpectedMessage {
            expected: message,
            actual: response.meta.message_id,
        }
        .into());
    }
    if response.meta.message_control_flags & 0x02 == 0 || response.is_indication() {
        return Err(CodecError::MalformedField("QMI response control flag").into());
    }
    response.check_result()?;
    Ok(())
}

fn bool_field(response: &OwnedPacket, kind: u8) -> Option<bool> {
    response
        .find_tlv(kind)
        .and_then(|value| value.first())
        .map(|value| *value != 0)
}

fn parse_registration(
    response: &OwnedPacket,
) -> Result<ImsaRegistrationStatus, QmiServiceClientError> {
    let state = response
        .find_tlv(0x12)
        .ok_or(CodecError::MissingField("IMSA registration state"))?;
    if state.len() < 4 {
        return Err(CodecError::MalformedField("IMSA registration state").into());
    }
    let error_code = response
        .find_tlv(0x11)
        .map(|value| {
            if value.len() < 2 {
                return Err(CodecError::MalformedField("IMSA error code"));
            }
            Ok(u16::from_le_bytes([value[0], value[1]]))
        })
        .transpose()?;
    let technology = response
        .find_tlv(0x14)
        .map(|value| {
            if value.len() < 4 {
                return Err(CodecError::MalformedField("IMSA registration technology"));
            }
            Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
        })
        .transpose()?;
    let error_message = response
        .find_tlv(0x13)
        .map(|value| String::from_utf8_lossy(value).into_owned());
    Ok(ImsaRegistrationStatus {
        state: u32::from_le_bytes([state[0], state[1], state[2], state[3]]),
        error_code,
        error_message,
        technology,
    })
}

fn settings_tlvs(update: ImsSettingsUpdate) -> Result<Vec<(u8, u8)>, CodecError> {
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
    Ok(fields)
}

fn push_bool(fields: &mut Vec<(u8, u8)>, kind: u8, value: Option<bool>) {
    if let Some(value) = value {
        fields.push((kind, u8::from(value)));
    }
}
