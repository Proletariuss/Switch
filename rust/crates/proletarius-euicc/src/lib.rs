// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! Bounded, transport-independent GSMA eUICC profile operations.
//!
//! The crate owns the ES10c request/response codec and the `80 E2` APDU
//! segmentation rules.  A reader, QMI UIM channel, or another card backend
//! only implements [`CardTransport`]; it does not need to know about BER-TLV,
//! logical-channel CLA encoding, or `GET RESPONSE` handling.

#![no_std]

extern crate alloc;

use alloc::{string::String, vec::Vec};
use core::{fmt, mem};

use proletarius_ccid::{
    Error as CardError,
    apdu::{Response as ApduResponse, logical_channel_cla},
    bertlv::{self, Tlv},
};

mod profile_package;
mod rsp;

pub use profile_package::{
    MAX_BOUND_PROFILE_PACKAGE_SIZE, ProfileInstallOutcome, ProfileInstallResponse,
};
pub use rsp::{
    CancelSessionReason, InstalledProfile, PendingNotification, PrepareDownloadRequest,
    ServerAuthenticationRequest, confirmation_code_required,
};

pub use proletarius_ccid::CardTransport;

/// The GSMA ISD-R application used by SGP.22 eUICC administration.
pub const GSMA_ISDR_APPLICATION_AID: &[u8] = &[
    0xa0, 0x00, 0x00, 0x05, 0x59, 0x10, 0x10, 0xff, 0xff, 0xff, 0xff, 0x89, 0x00, 0x00, 0x01, 0x00,
];

/// Known vendor ISD-R AIDs probed by the caller-driven card discovery path.
/// The list is ordered so the standard GSMA application is tried first after
/// the eSTK dual-slot applications.  Opening an AID remains the caller's
/// responsibility; this crate never scans or allocates logical channels.
pub const ESTKME_SE0_APPLICATION_AID: &[u8] = &[
    0xa0, 0x65, 0x73, 0x74, 0x6b, 0x6d, 0x65, 0xff, 0xff, 0x49, 0x53, 0x44, 0x2d, 0x52, 0x20, 0x30,
];
pub const ESTKME_SE1_APPLICATION_AID: &[u8] = &[
    0xa0, 0x65, 0x73, 0x74, 0x6b, 0x6d, 0x65, 0xff, 0xff, 0x49, 0x53, 0x44, 0x2d, 0x52, 0x20, 0x31,
];
pub const ESIMME_APPLICATION_AID: &[u8] = &[
    0xa0, 0x00, 0x00, 0x05, 0x59, 0x10, 0x10, 0x00, 0x00, 0x00, 0x00, 0x89, 0x00, 0x00, 0x03, 0x00,
];
pub const ESIMME_V2_APPLICATION_AID: &[u8] = &[
    0xa0, 0x00, 0x00, 0x05, 0x59, 0x10, 0x10, 0x00, 0x00, 0x00, 0x89, 0x00, 0x00, 0x00, 0x03, 0x00,
];
pub const FIVEBER_APPLICATION_AID: &[u8] = &[
    0xa0, 0x00, 0x00, 0x05, 0x59, 0x10, 0x10, 0xff, 0xff, 0xff, 0xff, 0x89, 0x00, 0x05, 0x05, 0x00,
];
pub const XESIM_APPLICATION_AID: &[u8] = &[
    0xa0, 0x00, 0x00, 0x05, 0x59, 0x10, 0x10, 0xff, 0xff, 0xff, 0xff, 0x89, 0x00, 0x00, 0x01, 0x77,
];
pub const LINKSFIELD_APPLICATION_AID: &[u8] = &[
    0xa0, 0x00, 0x00, 0x05, 0x59, 0x10, 0x4c, 0x69, 0x6e, 0x6b, 0x73, 0x66, 0x69, 0x65, 0x6c, 0x64,
];
pub const GLOCALME_APPLICATION_AID: &[u8] = &[
    0xa0, 0x00, 0x00, 0x06, 0x28, 0x10, 0x10, 0xff, 0xff, 0xff, 0xff, 0x89, 0x00, 0x00, 0x01, 0x00,
];
pub const ESTKME_AUX_APPLICATION_AID: &[u8] = &[
    0xa0, 0x65, 0x73, 0x74, 0x6b, 0x6d, 0x65, 0xff, 0xff, 0xff, 0xff, 0x49, 0x53, 0x44, 0x2d, 0x52,
];
pub const ESTKME_PRODUCT_APPLICATION_AID: &[u8] = &[
    0xa0, 0x65, 0x73, 0x74, 0x6b, 0x6d, 0x65, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x6d, 0x67, 0x74,
];

pub const KNOWN_ISDR_APPLICATION_AIDS: &[&[u8]] = &[
    ESTKME_SE0_APPLICATION_AID,
    ESTKME_SE1_APPLICATION_AID,
    GSMA_ISDR_APPLICATION_AID,
    ESIMME_APPLICATION_AID,
    ESIMME_V2_APPLICATION_AID,
    FIVEBER_APPLICATION_AID,
    XESIM_APPLICATION_AID,
    LINKSFIELD_APPLICATION_AID,
    GLOCALME_APPLICATION_AID,
    ESTKME_AUX_APPLICATION_AID,
];

/// Embedded certificate issuer labels used by the reader eSIM overview.
pub const PKI_CERTIFICATE_ISSUERS_JSON: &str = include_str!("../resources/pki/ci.json");
/// Embedded EUM/accreditation data used by the reader eSIM overview.
pub const PKI_ACCREDITED_SUPPLIERS_JSON: &str = include_str!("../resources/pki/accredited.json");

/// Default maximum segment size used by the eUICC admin protocol.
pub const DEFAULT_MSS: usize = 254;
pub const MAX_MSS: usize = 254;
const MAX_REQUEST_SIZE: usize = 65_535;

const MAX_RESPONSE_SIZE: usize = 65_535;
const MAX_GET_RESPONSE_ROUNDS: usize = 256;
const MAX_PRODUCT_RESPONSE_ROUNDS: usize = 256;
const MAX_PROFILE_IDENTIFIER_BYTES: usize = 32;
const PROFILE_FIELD_COUNT: usize = 12;
const MAX_PROFILE_ICCID_BYTES: usize = 16;
const MAX_PROFILE_AID_BYTES: usize = 32;
const MAX_PROFILE_TEXT_BYTES: usize = 512;
const MAX_PROFILE_ICON_BYTES: usize = 16 * 1024;
const MAX_PROFILE_OWNER_BYTES: usize = 64;
// A profile record may contain optional SGP.22/vendor metadata beyond the
// requested tag set (B6/B7, policy, RPM, enterprise data, etc.). Keep the
// parser bounded without incorrectly rejecting otherwise valid records.
const MAX_PROFILE_CHILDREN: usize = 32;
const MAX_PROFILE_NOTIFICATION_CONFIGURATIONS: usize = 16;
const MAX_PROFILE_NOTIFICATION_EVENT_BYTES: usize = 8;
const MAX_PROFILE_NOTIFICATION_ADDRESS_BYTES: usize = 512;
/// EID length defined by SGP.22, before hexadecimal presentation encoding.
pub const EID_LENGTH_BYTES: usize = 16;
/// Maximum number of profile records accepted from one GetProfilesInfo reply.
///
/// Real eUICCs expose far fewer profiles.  Keeping this independent from the
/// byte-size limit prevents a malformed card response made of tiny records
/// from forcing an unbounded number of `ProfileInfo` allocations.
pub const MAX_PROFILE_COUNT: usize = 256;

const TAG_PROFILE_LIST: &[u8] = &[0xbf, 0x2d];
const TAG_PROFILE_LIST_CONTAINER: &[u8] = &[0xa0];
const TAG_PROFILE_LIST_ERROR: &[u8] = &[0x81];
const TAG_ENABLE_PROFILE: &[u8] = &[0xbf, 0x31];
const TAG_DISABLE_PROFILE: &[u8] = &[0xbf, 0x32];
const TAG_DELETE_PROFILE: &[u8] = &[0xbf, 0x33];
const TAG_GET_EID: &[u8] = &[0xbf, 0x3e];
const TAG_PROFILE: &[u8] = &[0xe3];
const TAG_PROFILE_ALT: &[u8] = &[0xbf, 0x25];
const TAG_RESULT: &[u8] = &[0x80];
const TAG_IDENTIFIER: &[u8] = &[0x5a];
const TAG_ISDP_AID: &[u8] = &[0x4f];
const TAG_REQUESTED_TAGS: &[u8] = &[0x5c];
const TAG_PROFILE_STATE: &[u8] = &[0x9f, 0x70];
const TAG_NICKNAME: &[u8] = &[0x90];
const TAG_SERVICE_PROVIDER_NAME: &[u8] = &[0x91];
const TAG_PROFILE_NAME: &[u8] = &[0x92];
const TAG_ICON_TYPE: &[u8] = &[0x93];
const TAG_ICON: &[u8] = &[0x94];
const TAG_PROFILE_CLASS: &[u8] = &[0x95];
const TAG_NOTIFICATION_CONFIGURATION: &[u8] = &[0xb6];
const TAG_NOTIFICATION_CONFIGURATION_ENTRY: &[u8] = &[0x30];
const TAG_NOTIFICATION_CONFIGURATION_EVENT: &[u8] = &[0x80];
const TAG_NOTIFICATION_CONFIGURATION_ADDRESS: &[u8] = &[0x81];
const TAG_PROFILE_OWNER: &[u8] = &[0xb7];
const TAG_PROFILE_POLICY_RULES: &[u8] = &[0x99];
const TAG_PROFILE_OWNER_MCC_MNC: &[u8] = &[0x80];
const TAG_PROFILE_OWNER_GID1: &[u8] = &[0x81];
const TAG_PROFILE_OWNER_GID2: &[u8] = &[0x82];

// ES10b/ES10a administration tags.  Keep these as byte slices rather than
// numeric tags: a number of eUICCs use the high-tag BER form and comparing
// the encoded bytes makes the parser reject ambiguous encodings.
const TAG_EUICC_INFO1: &[u8] = &[0xbf, 0x20];
const TAG_EUICC_INFO2: &[u8] = &[0xbf, 0x22];
const TAG_CONFIGURED_ADDRESSES: &[u8] = &[0xbf, 0x3c];
const TAG_LIST_NOTIFICATION: &[u8] = &[0xbf, 0x28];
const TAG_SET_NICKNAME: &[u8] = &[0xbf, 0x29];
const TAG_NOTIFICATION_METADATA: &[u8] = &[0xbf, 0x2f];
const TAG_REMOVE_NOTIFICATION: &[u8] = &[0xbf, 0x30];
const TAG_NOTIFICATION_LIST: &[u8] = &[0xa0];
const TAG_NOTIFICATION_LIST_ERROR: &[u8] = &[0x81];
const TAG_SEQUENCE_NUMBER: &[u8] = &[0x80];
const TAG_NOTIFICATION_EVENT: &[u8] = &[0x81];
const TAG_NOTIFICATION_ADDRESS: &[u8] = &[0x0c];
/// SGP.22 EUICCInfo2 `extCardResource`: a constructed context-4 field.
const TAG_INFO_EXT_RESOURCE: &[u8] = &[0xa4];
/// Some cards publish the same content as a primitive context-4 field, which
/// the reference reader tolerates as well.
const TAG_INFO_EXT_RESOURCE_LEGACY: &[u8] = &[0x84];
const TAG_INFO_FIRMWARE: &[u8] = &[0x83];
const TAG_INFO_SAS: &[u8] = &[0x0c];
const TAG_INFO_CI_LIST: &[u8] = &[0xaa];
const TAG_INFO_FORBIDDEN_PPR: &[u8] = &[0x99];
const TAG_INFO_FREE_NVRAM: &[u8] = &[0x82];
const TAG_ADDRESS_DEFAULT_DP: &[u8] = &[0x80];
const TAG_ADDRESS_ROOT_DS: &[u8] = &[0x81];
const MAX_EUICC_TEXT_BYTES: usize = 512;
const MAX_EUICC_FIRMWARE_BYTES: usize = 16;
const MAX_EUICC_CERTIFICATE_IDS: usize = 64;
const MAX_EUICC_CERTIFICATE_ID_BYTES: usize = 64;
/// Maximum number of pending notification records accepted from one card.
/// The application layer uses the same bound for its caller-owned buffer.
pub const MAX_NOTIFICATION_COUNT: usize = 256;
const MAX_NOTIFICATION_ADDRESS_BYTES: usize = 512;
const MAX_NOTIFICATION_ICCID_BYTES: usize = 16;
const MAX_NICKNAME_BYTES: usize = 64;

#[derive(Debug, Eq, PartialEq)]
pub enum Error<E> {
    Transport(E),
    Card(CardError),
    Invalid(&'static str),
    OutputTooSmall,
    ResponseTooLarge,
    ProfileList(u8),
    ProfileOperation {
        operation: ProfileOperation,
        result: ProfileOperationResult,
    },
    NicknameOperation {
        result: NicknameResult,
    },
    NotificationOperation {
        operation: NotificationOperation,
        result: NotificationResult,
    },
    ProfileInstall {
        command_id: u8,
        reason: u8,
    },
}

impl<E> From<CardError> for Error<E> {
    fn from(error: CardError) -> Self {
        Self::Card(error)
    }
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "eUICC transport error: {error}"),
            Self::Card(error) => error.fmt(formatter),
            Self::Invalid(reason) => write!(formatter, "invalid eUICC response: {reason}"),
            Self::OutputTooSmall => formatter.write_str("eUICC output buffer is too small"),
            Self::ResponseTooLarge => formatter.write_str("eUICC response exceeds the limit"),
            Self::ProfileList(code) => {
                write!(formatter, "eUICC profile list failed with code {code}")
            }
            Self::ProfileOperation { operation, result } => {
                write!(formatter, "{operation} failed: {result}")
            }
            Self::NicknameOperation { result } => {
                write!(formatter, "setNickname failed: {result}")
            }
            Self::NotificationOperation { operation, result } => {
                write!(formatter, "{operation} failed: {result}")
            }
            Self::ProfileInstall { command_id, reason } => write!(
                formatter,
                "loadBoundProfilePackage failed (command={command_id}, reason={reason})"
            ),
        }
    }
}

pub type Result<T, E> = core::result::Result<T, Error<E>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IccidError {
    InvalidLength,
    NonDecimal,
    InvalidBcd,
}

impl fmt::Display for IccidError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLength => "ICCID length is out of range",
            Self::NonDecimal => "ICCID is not decimal",
            Self::InvalidBcd => "ICCID contains invalid swapped BCD",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProfileOperation {
    Enable = 49,
    Disable = 50,
    Delete = 51,
}

impl ProfileOperation {
    const fn tag(self) -> &'static [u8] {
        match self {
            Self::Enable => TAG_ENABLE_PROFILE,
            Self::Disable => TAG_DISABLE_PROFILE,
            Self::Delete => TAG_DELETE_PROFILE,
        }
    }
}

impl fmt::Display for ProfileOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Enable => "enableProfile",
            Self::Disable => "disableProfile",
            Self::Delete => "deleteProfile",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProfileOperationResult {
    Ok = 0,
    IccidOrAidNotFound = 1,
    ProfileInWrongState = 2,
    DisallowedByPolicy = 3,
    WrongProfileReenabling = 4,
    CatBusy = 5,
    /// The profile switch is rejected by an enterprise policy (SGP.22 code 6).
    DisallowedByEnterpriseRule = 6,
    /// The card could not execute the command (SGP.22 code 7).
    CommandError = 7,
    /// The profile switch is rejected by remote-profile-management policy
    /// (SGP.22 code 9).
    DisallowedForRpm = 9,
    /// No eSIM port is available for this profile (SGP.22 code 10).
    NoEsimPortAvailable = 10,
    Undefined = 127,
    Unknown(u8),
}

impl ProfileOperationResult {
    const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Ok,
            1 => Self::IccidOrAidNotFound,
            2 => Self::ProfileInWrongState,
            3 => Self::DisallowedByPolicy,
            4 => Self::WrongProfileReenabling,
            5 => Self::CatBusy,
            6 => Self::DisallowedByEnterpriseRule,
            7 => Self::CommandError,
            9 => Self::DisallowedForRpm,
            10 => Self::NoEsimPortAvailable,
            127 => Self::Undefined,
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::IccidOrAidNotFound => 1,
            Self::ProfileInWrongState => 2,
            Self::DisallowedByPolicy => 3,
            Self::WrongProfileReenabling => 4,
            Self::CatBusy => 5,
            Self::DisallowedByEnterpriseRule => 6,
            Self::CommandError => 7,
            Self::DisallowedForRpm => 9,
            Self::NoEsimPortAvailable => 10,
            Self::Undefined => 127,
            Self::Unknown(code) => code,
        }
    }
}

impl fmt::Display for ProfileOperationResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => formatter.write_str("ok"),
            Self::IccidOrAidNotFound => formatter.write_str("iccidOrAidNotFound"),
            Self::ProfileInWrongState => formatter.write_str("profileInWrongState"),
            Self::DisallowedByPolicy => formatter.write_str("disallowedByPolicy"),
            Self::WrongProfileReenabling => formatter.write_str("wrongProfileReenabling"),
            Self::CatBusy => formatter.write_str("catBusy"),
            Self::DisallowedByEnterpriseRule => formatter.write_str("disallowedByEnterpriseRule"),
            Self::CommandError => formatter.write_str("commandError"),
            Self::DisallowedForRpm => formatter.write_str("disallowedForRpm"),
            Self::NoEsimPortAvailable => formatter.write_str("noEsimPortAvailable"),
            Self::Undefined => formatter.write_str("undefinedError"),
            Self::Unknown(code) => write!(formatter, "unknown({code})"),
        }
    }
}

/// Result codes returned by ES10c.SetNickname.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NicknameResult {
    Ok,
    IccidNotFound,
    Undefined,
    Unknown(u8),
}

impl NicknameResult {
    const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Ok,
            1 => Self::IccidNotFound,
            127 => Self::Undefined,
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::IccidNotFound => 1,
            Self::Undefined => 127,
            Self::Unknown(code) => code,
        }
    }
}

impl fmt::Display for NicknameResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Ok => "ok",
            Self::IccidNotFound => "iccidNotFound",
            Self::Undefined => "undefinedError",
            Self::Unknown(_) => "unknown",
        })
    }
}

/// ES10b notification operations exposed by this crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationOperation {
    List,
    Retrieve,
    Remove,
}

impl fmt::Display for NotificationOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::List => "listNotification",
            Self::Retrieve => "retrieveNotificationsList",
            Self::Remove => "removeNotification",
        })
    }
}

/// Result codes returned by ES10b notification operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationResult {
    Ok,
    NotFound,
    Undefined,
    Unknown(u8),
}

impl NotificationResult {
    const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Ok,
            1 => Self::NotFound,
            127 => Self::Undefined,
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::NotFound => 1,
            Self::Undefined => 127,
            Self::Unknown(code) => code,
        }
    }
}

impl fmt::Display for NotificationResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Ok => "ok",
            Self::NotFound => "notFound",
            Self::Undefined => "undefinedError",
            Self::Unknown(_) => "unknown",
        })
    }
}

/// Version selector for ES10b.GetEUICCInfo.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EuiccInfoVersion {
    V1,
    V2,
}

impl EuiccInfoVersion {
    const fn tag(self) -> &'static [u8] {
        match self {
            Self::V1 => TAG_EUICC_INFO1,
            Self::V2 => TAG_EUICC_INFO2,
        }
    }

    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }
}

/// Parsed ES10b EUICCInfo1/2 metadata.  All fields are bounded by the parser
/// and owned by the caller, so the value can safely outlive the card session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EuiccInfo {
    pub version: u8,
    pub free_nvram_bytes: Option<u64>,
    pub firmware: Vec<u8>,
    pub sas_accreditation_number: Vec<u8>,
    pub certificate_key_ids: Vec<Vec<u8>>,
    /// Raw forbidden Profile Policy Rule identifiers (ASN.1 BIT STRING).
    pub forbidden_profile_policy_rules: Vec<u8>,
}

/// Compatibility spelling used by the Go implementation and API layer.
pub type EUICCInfo = EuiccInfo;

impl EuiccInfo {
    #[must_use]
    pub fn firmware_string(&self) -> Option<String> {
        if self.firmware.is_empty() {
            return None;
        }
        if self.firmware.len() == 3 {
            return Some(format_decimal_triplet(&self.firmware));
        }
        String::from_utf8(self.firmware.clone()).ok()
    }

    #[must_use]
    pub fn sas_accreditation_number_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.sas_accreditation_number).ok()
    }
}

/// Addresses returned by ES10a.GetEUICCConfiguredAddresses.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfiguredAddresses {
    pub default_smdp_address: Vec<u8>,
    pub root_smds_address: Vec<u8>,
}

impl ConfiguredAddresses {
    #[must_use]
    pub fn default_smdp_address_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.default_smdp_address).ok()
    }

    #[must_use]
    pub fn root_smds_address_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.root_smds_address).ok()
    }
}

/// One ES10b notification metadata record.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NotificationInfo {
    pub sequence_number: u64,
    pub event: u8,
    pub address: Vec<u8>,
    pub iccid: Vec<u8>,
}

/// eSTK.me product-application fields.  These are intentionally kept outside
/// the GSMA profile API: callers must open the vendor product AID explicitly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductField {
    SkuName,
    SerialNumber,
    Bootloader,
    Firmware,
}

impl ProductField {
    const fn p1(self) -> u8 {
        match self {
            Self::SkuName => 0x03,
            Self::SerialNumber => 0x00,
            Self::Bootloader => 0x01,
            Self::Firmware => 0x02,
        }
    }
}

fn format_decimal_triplet(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2 - 1);
    for (index, byte) in bytes.iter().copied().enumerate() {
        if index != 0 {
            output.push('.');
        }
        if byte >= 100 {
            output.push(char::from(b'0' + byte / 100));
            output.push(char::from(b'0' + (byte / 10) % 10));
            output.push(char::from(b'0' + byte % 10));
        } else if byte >= 10 {
            output.push(char::from(b'0' + byte / 10));
            output.push(char::from(b'0' + byte % 10));
        } else {
            output.push(char::from(b'0' + byte));
        }
    }
    output
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProfileState {
    #[default]
    Disabled,
    Enabled,
    Unknown(u8),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProfileClass {
    Test,
    #[default]
    Provisioning,
    Operational,
    Unknown(u8),
}

/// Fields that can be requested by `ES10c.GetProfilesInfo`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileField {
    Iccid,
    IsdpAid,
    State,
    Nickname,
    ServiceProviderName,
    Name,
    IconType,
    Icon,
    Class,
    NotificationConfiguration,
    ProfileOwner,
    ProfilePolicyRules,
}

impl ProfileField {
    const fn tag(self) -> &'static [u8] {
        match self {
            Self::Iccid => TAG_IDENTIFIER,
            Self::IsdpAid => TAG_ISDP_AID,
            Self::State => TAG_PROFILE_STATE,
            Self::Nickname => TAG_NICKNAME,
            Self::ServiceProviderName => TAG_SERVICE_PROVIDER_NAME,
            Self::Name => TAG_PROFILE_NAME,
            Self::IconType => TAG_ICON_TYPE,
            Self::Icon => TAG_ICON,
            Self::Class => TAG_PROFILE_CLASS,
            Self::NotificationConfiguration => TAG_NOTIFICATION_CONFIGURATION,
            Self::ProfileOwner => TAG_PROFILE_OWNER,
            Self::ProfilePolicyRules => TAG_PROFILE_POLICY_RULES,
        }
    }

    const fn bit(self) -> u16 {
        1 << self as u8
    }
}

const DEFAULT_PROFILE_FIELDS: &[ProfileField] = &[
    ProfileField::Iccid,
    ProfileField::IsdpAid,
    ProfileField::State,
    ProfileField::Nickname,
    ProfileField::ServiceProviderName,
    ProfileField::Name,
    // The reference LPA reader requests the icon tags with the same list so
    // the UI can render the carrier icon the card publishes.
    ProfileField::IconType,
    ProfileField::Icon,
    ProfileField::Class,
    ProfileField::NotificationConfiguration,
    ProfileField::ProfileOwner,
    ProfileField::ProfilePolicyRules,
];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProfileNotificationConfiguration {
    /// Raw ASN.1 BIT STRING content for the profile-management event mask.
    pub events: Vec<u8>,
    pub address: Vec<u8>,
}

impl ProfileNotificationConfiguration {
    #[must_use]
    pub fn address_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.address).ok()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProfileInfo {
    /// Raw GSM-swapped BCD ICCID bytes, as returned by the eUICC.
    pub iccid: Vec<u8>,
    pub isdp_aid: Vec<u8>,
    pub state: ProfileState,
    pub nickname: Vec<u8>,
    pub service_provider_name: Vec<u8>,
    pub profile_name: Vec<u8>,
    pub icon_type: u8,
    pub icon: Vec<u8>,
    pub class: ProfileClass,
    pub notification_configurations: Vec<ProfileNotificationConfiguration>,
    /// Raw TS 24.008 MCC/MNC encoding from `profileOwner`.
    pub mcc_mnc: Vec<u8>,
    pub gid1: Vec<u8>,
    pub gid2: Vec<u8>,
    /// Raw Profile Policy Rules bit string (tag 99), when published by the
    /// eUICC.  The first byte is the ASN.1 unused-bit count.
    pub profile_policy_rules: Vec<u8>,
}

impl ProfileInfo {
    /// Decode the ICCID into its display form without retaining another copy.
    pub fn iccid_string(&self) -> core::result::Result<String, IccidError> {
        decode_swapped_bcd(&self.iccid)
    }

    #[must_use]
    pub fn nickname_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.nickname).ok()
    }

    #[must_use]
    pub fn service_provider_name_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.service_provider_name).ok()
    }

    #[must_use]
    pub fn profile_name_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.profile_name).ok()
    }
}

/// A card profile identifier.  SGP.22 accepts either an ICCID or the ISD-P
/// AID for profile operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileIdentifier<'a> {
    Iccid(&'a [u8]),
    IsdpAid(&'a [u8]),
}

impl<'a> ProfileIdentifier<'a> {
    const fn tag(self) -> &'static [u8] {
        match self {
            Self::Iccid(_) => TAG_IDENTIFIER,
            Self::IsdpAid(_) => TAG_ISDP_AID,
        }
    }

    const fn value(self) -> &'a [u8] {
        match self {
            Self::Iccid(value) | Self::IsdpAid(value) => value,
        }
    }
}

/// ES10c profile administration over an already-open logical card channel.
pub struct Euicc<T> {
    transport: T,
    logical_channel: u8,
    mss: usize,
    request: Vec<u8>,
    apdu: Vec<u8>,
    raw_response: Vec<u8>,
    payload: Vec<u8>,
}

impl<T: CardTransport> Euicc<T> {
    pub fn new(transport: T, logical_channel: u8, mss: usize) -> Result<Self, T::Error> {
        let mss = if mss == 0 { DEFAULT_MSS } else { mss };
        if mss > MAX_MSS {
            return Err(Error::Invalid("eUICC MSS must be between 1 and 254"));
        }
        logical_channel_cla(0x80, logical_channel).map_err(Error::Card)?;
        Ok(Self {
            transport,
            logical_channel,
            mss,
            request: Vec::with_capacity(256),
            apdu: Vec::with_capacity(MAX_MSS + 5),
            raw_response: Vec::with_capacity(258),
            payload: Vec::with_capacity(256),
        })
    }

    pub fn with_default_mss(transport: T, logical_channel: u8) -> Result<Self, T::Error> {
        Self::new(transport, logical_channel, DEFAULT_MSS)
    }

    #[must_use]
    pub const fn logical_channel(&self) -> u8 {
        self.logical_channel
    }

    #[must_use]
    pub const fn mss(&self) -> usize {
        self.mss
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Send the 9eSIM B0 pre-download BF71 authentication command on the
    /// currently selected logical channel. This deliberately accepts only
    /// the fixed 27-byte BF71 TLV; it is not a general vendor APDU
    /// escape hatch.
    pub fn transmit_b0_authentication(&mut self, body: &[u8]) -> Result<Vec<u8>, T::Error> {
        if body.len() != 27 {
            return Err(Error::Invalid("B0 authentication body must be 27 bytes"));
        }
        let cla = logical_channel_cla(0x80, self.logical_channel).map_err(Error::Card)?;
        self.apdu.clear();
        self.apdu
            .extend_from_slice(&[cla, 0xe2, 0x91, 0x00, body.len() as u8]);
        self.apdu.extend_from_slice(body);
        self.raw_response.clear();
        self.transport
            .transmit(&self.apdu, &mut self.raw_response)
            .map_err(Error::Transport)?;
        Ok(self.raw_response.clone())
    }

    pub fn into_inner(self) -> T {
        self.transport
    }

    /// Request the profile fields needed by the normal eSIM list view.
    ///
    /// The caller owns the output storage and can reuse it after every
    /// profile operation.  The returned count is the authoritative number of
    /// records; entries beyond it are left untouched.  When the buffer is too
    /// small or a record is malformed, no existing output entry is modified.
    pub fn list_profiles_into(&mut self, output: &mut [ProfileInfo]) -> Result<usize, T::Error> {
        self.list_profiles_with_fields_into(DEFAULT_PROFILE_FIELDS, output)
    }

    pub fn list_profiles_with_fields_into(
        &mut self,
        fields: &[ProfileField],
        output: &mut [ProfileInfo],
    ) -> Result<usize, T::Error> {
        self.build_profile_list_request(fields)?;
        self.exchange_request()?;
        parse_profile_list_into(&self.payload, output)
    }

    /// Enable a profile and write the authoritative profile list afterwards.
    ///
    /// The `refresh` flag is the ES10c refresh qualifier sent to the card.  It
    /// is separate from the list refresh performed here: callers always get a
    /// post-operation snapshot and never have to issue a second refresh call.
    pub fn enable_profile(
        &mut self,
        identifier: ProfileIdentifier<'_>,
        refresh: bool,
        output: &mut [ProfileInfo],
    ) -> Result<usize, T::Error> {
        self.enable_profile_command(identifier, refresh)?;
        self.list_profiles_into(output)
    }

    /// Execute ES10c.EnableProfile without the mandatory post-command list.
    ///
    /// A refresh-qualified enable can reset the UICC before a new logical
    /// channel is available. Callers that need to treat that reset as an
    /// accepted switch can therefore observe the command result separately
    /// from the best-effort profile refresh.
    pub fn enable_profile_command(
        &mut self,
        identifier: ProfileIdentifier<'_>,
        refresh: bool,
    ) -> Result<(), T::Error> {
        self.profile_operation(ProfileOperation::Enable, identifier, Some(refresh))
    }

    /// Execute ES10c.EnableProfile while preserving every nonzero card result.
    ///
    /// Unlike the compatibility command, a wrong-state response does not prove
    /// that the target was enabled. Callers must reconcile the card state.
    pub fn enable_profile_command_strict(
        &mut self,
        identifier: ProfileIdentifier<'_>,
        refresh: bool,
    ) -> Result<(), T::Error> {
        self.profile_operation_with_compatibility(
            ProfileOperation::Enable,
            identifier,
            Some(refresh),
            false,
        )
    }

    /// Disable a profile and write the authoritative profile list afterwards.
    pub fn disable_profile(
        &mut self,
        identifier: ProfileIdentifier<'_>,
        refresh: bool,
        output: &mut [ProfileInfo],
    ) -> Result<usize, T::Error> {
        self.disable_profile_command(identifier, refresh)?;
        self.list_profiles_into(output)
    }

    /// Execute ES10c.DisableProfile without refreshing the profile list.
    pub fn disable_profile_command(
        &mut self,
        identifier: ProfileIdentifier<'_>,
        refresh: bool,
    ) -> Result<(), T::Error> {
        self.profile_operation(ProfileOperation::Disable, identifier, Some(refresh))
    }

    /// Delete a profile and write the authoritative profile list afterwards.
    pub fn delete_profile(
        &mut self,
        identifier: ProfileIdentifier<'_>,
        output: &mut [ProfileInfo],
    ) -> Result<usize, T::Error> {
        self.delete_profile_command(identifier)?;
        self.list_profiles_into(output)
    }

    /// Execute ES10c.DeleteProfile without refreshing the profile list.
    pub fn delete_profile_command(
        &mut self,
        identifier: ProfileIdentifier<'_>,
    ) -> Result<(), T::Error> {
        self.profile_operation(ProfileOperation::Delete, identifier, None)
    }

    /// Execute one complete profile state change using the bounded fallback
    /// policy required by removable eUICCs.
    ///
    /// ICCID remains the caller-facing identifier.  The current profile list
    /// is read once to resolve its ISD-P AID, then cards which reject ICCID
    /// addressing can be retried once with that AID.  The refresh qualifier
    /// is never changed as part of identifier fallback because it cannot make
    /// an unknown identifier valid. `catBusy` is retried at most three times
    /// per candidate. The callback is invoked before each retry
    /// so a platform runtime can provide the required delay without making
    /// this transport-independent crate depend on an operating system clock.
    /// A successful command is always followed by an authoritative profile
    /// list refresh.
    pub fn apply_profile_operation(
        &mut self,
        operation: ProfileOperation,
        identifier: ProfileIdentifier<'_>,
        refresh: bool,
        output: &mut [ProfileInfo],
        before_cat_busy_retry: impl FnMut(usize),
    ) -> Result<usize, T::Error> {
        self.apply_profile_operation_with(
            operation,
            identifier,
            refresh,
            output,
            true,
            before_cat_busy_retry,
        )
    }

    /// Execute one profile state change without reading the profile list
    /// afterwards.
    ///
    /// A switch makes the card refresh itself, and a transport that carries
    /// the APDU stream directly (a module's `AT+CGLA` bridge) loses its
    /// logical channel while that refresh runs.  Callers behind such a
    /// transport observe the accepted state change here and re-read the card
    /// on a fresh session, instead of turning an accepted switch into a
    /// reported failure.
    pub fn apply_profile_state_change(
        &mut self,
        operation: ProfileOperation,
        identifier: ProfileIdentifier<'_>,
        refresh: bool,
        output: &mut [ProfileInfo],
        before_cat_busy_retry: impl FnMut(usize),
    ) -> Result<usize, T::Error> {
        self.apply_profile_operation_with(
            operation,
            identifier,
            refresh,
            output,
            false,
            before_cat_busy_retry,
        )
    }

    fn apply_profile_operation_with(
        &mut self,
        operation: ProfileOperation,
        identifier: ProfileIdentifier<'_>,
        refresh: bool,
        output: &mut [ProfileInfo],
        list_after: bool,
        mut before_cat_busy_retry: impl FnMut(usize),
    ) -> Result<usize, T::Error> {
        match self.apply_profile_candidate(
            operation,
            identifier,
            refresh,
            &mut before_cat_busy_retry,
        ) {
            Ok(()) => self.finish_profile_operation(list_after, output),
            Err(error) => {
                let mut listed_count = None;
                // A card may have accepted the state change but return an
                // unusable/undefined result while it starts its refresh. A
                // fresh profile list is safe and lets us avoid submitting the
                // same state-changing APDU twice. Do not probe after other
                // command errors: those are definitive.
                if list_after
                    && profile_error_is_undefined(&error)
                    && let Ok(count) = self.list_profiles_into(output)
                {
                    listed_count = Some(count);
                    if profile_operation_state_matches(operation, identifier, &output[..count]) {
                        return Ok(count);
                    }
                }

                // Some eUICCs (notably carrier-issued O2 profiles) reject the
                // soft-refresh form with `undefinedError` even though the
                // profile identifier is valid.  Retry once with the standard
                // refresh qualifier before trying an alternate identifier;
                // this is a different command variant, not a blind replay.
                if !refresh
                    && profile_error_is_undefined(&error)
                    && self
                        .apply_profile_candidate(
                            operation,
                            identifier,
                            true,
                            &mut before_cat_busy_retry,
                        )
                        .is_ok()
                {
                    return self.finish_profile_operation(list_after, output);
                }

                if !profile_error_allows_identifier_fallback(&error)
                    && !(profile_error_is_undefined(&error)
                        && matches!(identifier, ProfileIdentifier::Iccid(_)))
                {
                    return Err(error);
                }
                let ProfileIdentifier::Iccid(iccid) = identifier else {
                    return Err(error);
                };
                let count = listed_count
                    .unwrap_or_else(|| self.list_profiles_into(output).unwrap_or(0))
                    .min(output.len());
                let isdp_aid = output[..count]
                    .iter()
                    .find(|profile| iccid_matches(&profile.iccid, iccid))
                    .filter(|profile| !profile.isdp_aid.is_empty())
                    .map(|profile| profile.isdp_aid.clone());
                let Some(isdp_aid) = isdp_aid else {
                    return Err(error);
                };

                let aid_refresh = refresh || profile_error_is_undefined(&error);
                self.apply_profile_candidate(
                    operation,
                    ProfileIdentifier::IsdpAid(&isdp_aid),
                    aid_refresh,
                    &mut before_cat_busy_retry,
                )?;
                self.finish_profile_operation(list_after, output)
            }
        }
    }

    fn finish_profile_operation(
        &mut self,
        list_after: bool,
        output: &mut [ProfileInfo],
    ) -> Result<usize, T::Error> {
        if list_after {
            self.list_profiles_into(output)
        } else {
            Ok(0)
        }
    }

    fn apply_profile_candidate(
        &mut self,
        operation: ProfileOperation,
        identifier: ProfileIdentifier<'_>,
        refresh: bool,
        before_cat_busy_retry: &mut impl FnMut(usize),
    ) -> Result<(), T::Error> {
        // Match NekokoLPA2's bounded profile-switch recovery: CAT_BUSY is a
        // card lifecycle state, not a transport failure.  Retry on the same
        // operation up to ten times, with the runtime supplying the delay.
        const MAX_CAT_BUSY_RETRIES: usize = 10;
        for attempt in 0..=MAX_CAT_BUSY_RETRIES {
            let result = match operation {
                ProfileOperation::Enable => self.enable_profile_command(identifier, refresh),
                ProfileOperation::Disable => self.disable_profile_command(identifier, refresh),
                ProfileOperation::Delete => self.delete_profile_command(identifier),
            };
            match result {
                Ok(()) => return Ok(()),
                Err(error)
                    if attempt < MAX_CAT_BUSY_RETRIES && profile_error_is_cat_busy(&error) =>
                {
                    before_cat_busy_retry(attempt + 1);
                }
                Err(error) => return Err(error),
            }
        }
        Err(Error::Invalid("profile operation retry state is invalid"))
    }

    /// Read the EID into caller-owned storage and return its length.
    pub fn get_eid(&mut self, output: &mut [u8]) -> Result<usize, T::Error> {
        self.apdu.clear();
        bertlv::encode(TAG_REQUESTED_TAGS, TAG_IDENTIFIER, &mut self.apdu).map_err(Error::Card)?;
        self.request.clear();
        bertlv::encode_constructed(TAG_GET_EID, &self.apdu, &mut self.request)
            .map_err(Error::Card)?;
        self.exchange_request()?;
        let outer = single_tlv(&self.payload).map_err(Error::Card)?;
        if outer.tag.bytes != TAG_GET_EID || !outer.tag.constructed {
            return Err(Error::Invalid("unexpected GetEID response tag"));
        }
        let eid = child(outer, TAG_IDENTIFIER)
            .map_err(Error::Card)?
            .ok_or(Error::Invalid("GetEID response has no EID"))?;
        if eid.value.len() != EID_LENGTH_BYTES {
            return Err(Error::Invalid("EID must contain exactly 16 bytes"));
        }
        if eid.value.len() > output.len() {
            return Err(Error::OutputTooSmall);
        }
        output[..eid.value.len()].copy_from_slice(eid.value);
        Ok(eid.value.len())
    }

    /// Read one field from the optional eSTK.me product application.
    ///
    /// The product application uses a small raw APDU (`00 00 P1 00 00`), but
    /// still runs on the logical channel owned by this session.  Keeping the
    /// command here avoids exposing transport or CLA details to the card
    /// integration layer.  No text decoding is performed; product firmware
    /// may return either ASCII or a vendor binary representation.
    pub fn read_product_field_into(
        &mut self,
        field: ProductField,
        output: &mut [u8],
    ) -> Result<usize, T::Error> {
        let cla = logical_channel_cla(0x00, self.logical_channel).map_err(Error::Card)?;
        self.payload.clear();
        for _ in 0..MAX_PRODUCT_RESPONSE_ROUNDS {
            self.apdu.clear();
            self.apdu
                .extend_from_slice(&[cla, 0x00, field.p1(), 0x00, 0x00]);
            let status = self.transmit_apdu()?;
            if status >> 8 == 0x6c {
                continue;
            }
            self.finish_status(status)?;
            if self.payload.len() > output.len() {
                return Err(Error::OutputTooSmall);
            }
            output[..self.payload.len()].copy_from_slice(&self.payload);
            return Ok(self.payload.len());
        }
        Err(Error::Invalid("too many eSTK product response segments"))
    }

    /// Read EUICCInfo2, or EUICCInfo1 when the card does not implement the
    /// newer response.  The fallback is intentionally explicit and bounded;
    /// an unsupported Info2 command must not leave a partially parsed value.
    pub fn get_euicc_info_preferred(&mut self) -> Result<EuiccInfo, T::Error> {
        match self.get_euicc_info(EuiccInfoVersion::V2) {
            Ok(info) => Ok(info),
            Err(Error::Card(CardError::CardStatus(0x6a81 | 0x6d00))) => {
                self.get_euicc_info(EuiccInfoVersion::V1)
            }
            Err(error) => Err(error),
        }
    }

    /// Read one version of ES10b.GetEUICCInfo.
    pub fn get_euicc_info(&mut self, version: EuiccInfoVersion) -> Result<EuiccInfo, T::Error> {
        self.request.clear();
        bertlv::encode_constructed(version.tag(), &[], &mut self.request).map_err(Error::Card)?;
        self.exchange_request()?;
        parse_euicc_info(&self.payload, version).map_err(Error::Card)
    }

    /// Read one version of ES10b.GetEUICCInfo into a reusable value.
    pub fn get_euicc_info_into(
        &mut self,
        version: EuiccInfoVersion,
        output: &mut EuiccInfo,
    ) -> Result<(), T::Error> {
        let info = self.get_euicc_info(version)?;
        *output = info;
        Ok(())
    }

    /// Read one NXP proprietary eUICC data object on the selected ISD-R channel.
    pub fn get_nxp_data_into(
        &mut self,
        selector: [u8; 2],
        output: &mut [u8],
    ) -> Result<usize, T::Error> {
        if !matches!(selector, [0xdf, 0x25] | [0xdf, 0x31]) {
            return Err(Error::Invalid("unsupported NXP data selector"));
        }
        let cla = logical_channel_cla(0x80, self.logical_channel).map_err(Error::Card)?;
        self.payload.clear();
        self.apdu.clear();
        self.apdu
            .extend_from_slice(&[cla, 0xca, 0x00, 0xfe, 0x02, selector[0], selector[1]]);
        let status = self.transmit_apdu()?;
        self.finish_status(status)?;
        if self.payload.len() > output.len() {
            return Err(Error::OutputTooSmall);
        }
        output[..self.payload.len()].copy_from_slice(&self.payload);
        Ok(self.payload.len())
    }

    /// Read the default SM-DP+ and root SM-DS addresses from ES10a.
    pub fn get_configured_addresses(&mut self) -> Result<ConfiguredAddresses, T::Error> {
        self.request.clear();
        bertlv::encode_constructed(TAG_CONFIGURED_ADDRESSES, &[], &mut self.request)
            .map_err(Error::Card)?;
        self.exchange_request()?;
        parse_configured_addresses(&self.payload).map_err(Error::Card)
    }

    /// Set a profile nickname through ES10c.SetNickname.
    pub fn set_nickname(
        &mut self,
        identifier: ProfileIdentifier<'_>,
        nickname: &[u8],
    ) -> Result<(), T::Error> {
        if identifier.value().is_empty() || identifier.value().len() > MAX_PROFILE_IDENTIFIER_BYTES
        {
            return Err(Error::Invalid("profile identifier size is out of range"));
        }
        if nickname.len() > MAX_NICKNAME_BYTES || core::str::from_utf8(nickname).is_err() {
            return Err(Error::Invalid(
                "profile nickname must be valid UTF-8 and at most 64 bytes",
            ));
        }
        self.apdu.clear();
        bertlv::encode(identifier.tag(), identifier.value(), &mut self.apdu)
            .map_err(Error::Card)?;
        // SGP.22 uses an application primitive ICCID (5A) for this field.  An
        // ISD-P AID is accepted as well for cards implementing the extension.
        bertlv::encode(TAG_NICKNAME, nickname, &mut self.apdu).map_err(Error::Card)?;
        self.request.clear();
        bertlv::encode_constructed(TAG_SET_NICKNAME, &self.apdu, &mut self.request)
            .map_err(Error::Card)?;
        self.exchange_request()?;
        let result = parse_nickname_result(&self.payload).map_err(Error::Card)?;
        if result != NicknameResult::Ok {
            return Err(Error::NicknameOperation { result });
        }
        Ok(())
    }

    /// List all pending notifications into caller-owned storage.
    pub fn list_notifications_into(
        &mut self,
        output: &mut [NotificationInfo],
    ) -> Result<usize, T::Error> {
        self.list_notifications_filtered_into(None, output)
    }

    /// List notifications, optionally supplying the four-bit event filter
    /// defined by SGP.22.  A zero/absent filter asks the card for all events.
    pub fn list_notifications_filtered_into(
        &mut self,
        event_filter: Option<u8>,
        output: &mut [NotificationInfo],
    ) -> Result<usize, T::Error> {
        self.request.clear();
        if let Some(filter) = event_filter {
            if filter & !0x0f != 0 {
                return Err(Error::Invalid("notification event filter has unknown bits"));
            }
            // A BIT STRING has an initial unused-bits count octet.  Four
            // event bits are represented in the low nibble of the next byte.
            bertlv::encode(TAG_NOTIFICATION_EVENT, &[4, filter << 4], &mut self.request)
                .map_err(Error::Card)?;
        }
        self.apdu.clear();
        bertlv::encode_constructed(TAG_LIST_NOTIFICATION, &self.request, &mut self.apdu)
            .map_err(Error::Card)?;
        mem::swap(&mut self.request, &mut self.apdu);
        self.exchange_request()?;
        parse_notification_list(&self.payload, output)
    }

    /// Remove one notification after it has been delivered to the server.
    pub fn remove_notification(&mut self, sequence_number: u64) -> Result<(), T::Error> {
        let mut integer = [0_u8; 9];
        let integer_len = encode_unsigned_integer(sequence_number, &mut integer);
        self.apdu.clear();
        bertlv::encode(TAG_SEQUENCE_NUMBER, &integer[..integer_len], &mut self.apdu)
            .map_err(Error::Card)?;
        self.request.clear();
        bertlv::encode_constructed(TAG_REMOVE_NOTIFICATION, &self.apdu, &mut self.request)
            .map_err(Error::Card)?;
        self.exchange_request()?;
        let result = parse_notification_remove_result(&self.payload).map_err(Error::Card)?;
        if result != NotificationResult::Ok {
            return Err(Error::NotificationOperation {
                operation: NotificationOperation::Remove,
                result,
            });
        }
        Ok(())
    }

    fn profile_operation(
        &mut self,
        operation: ProfileOperation,
        identifier: ProfileIdentifier<'_>,
        refresh: Option<bool>,
    ) -> Result<(), T::Error> {
        self.profile_operation_with_compatibility(operation, identifier, refresh, true)
    }

    fn profile_operation_with_compatibility(
        &mut self,
        operation: ProfileOperation,
        identifier: ProfileIdentifier<'_>,
        refresh: Option<bool>,
        accept_idempotent_target_state: bool,
    ) -> Result<(), T::Error> {
        if identifier.value().is_empty() || identifier.value().len() > MAX_PROFILE_IDENTIFIER_BYTES
        {
            return Err(Error::Invalid("profile identifier size is out of range"));
        }
        self.apdu.clear();
        bertlv::encode(identifier.tag(), identifier.value(), &mut self.apdu)
            .map_err(Error::Card)?;
        if let Some(refresh) = refresh {
            self.request.clear();
            bertlv::encode_constructed(&[0xa0], &self.apdu, &mut self.request)
                .map_err(Error::Card)?;
            bertlv::encode(
                &[0x81],
                &[if refresh { 0xff } else { 0x00 }],
                &mut self.request,
            )
            .map_err(Error::Card)?;
            self.apdu.clear();
            bertlv::encode_constructed(operation.tag(), &self.request, &mut self.apdu)
                .map_err(Error::Card)?;
            mem::swap(&mut self.request, &mut self.apdu);
        } else {
            self.request.clear();
            bertlv::encode_constructed(operation.tag(), &self.apdu, &mut self.request)
                .map_err(Error::Card)?;
        }
        self.exchange_request()?;
        let result = parse_operation_result(&self.payload, operation).map_err(Error::Card)?;
        let idempotent_target_state = accept_idempotent_target_state
            && result == ProfileOperationResult::ProfileInWrongState
            && matches!(
                operation,
                ProfileOperation::Enable | ProfileOperation::Disable
            );
        if result != ProfileOperationResult::Ok && !idempotent_target_state {
            return Err(Error::ProfileOperation { operation, result });
        }
        Ok(())
    }

    fn build_profile_list_request(&mut self, fields: &[ProfileField]) -> Result<(), T::Error> {
        if fields.len() > PROFILE_FIELD_COUNT {
            return Err(Error::Invalid("too many profile fields requested"));
        }
        self.apdu.clear();
        let mut seen = 0_u16;
        for &field in fields {
            if seen & field.bit() != 0 {
                continue;
            }
            seen |= field.bit();
            self.apdu.extend_from_slice(field.tag());
        }
        self.request.clear();
        if !self.apdu.is_empty() {
            bertlv::encode(TAG_REQUESTED_TAGS, &self.apdu, &mut self.request)
                .map_err(Error::Card)?;
        }
        self.apdu.clear();
        bertlv::encode_constructed(TAG_PROFILE_LIST, &self.request, &mut self.apdu)
            .map_err(Error::Card)?;
        mem::swap(&mut self.request, &mut self.apdu);
        Ok(())
    }

    fn exchange_request(&mut self) -> Result<(), T::Error> {
        if self.request.is_empty() || self.request.len() > MAX_REQUEST_SIZE {
            return Err(Error::Invalid("eUICC request size is out of range"));
        }
        let request = mem::take(&mut self.request);
        let result = self.exchange_bytes(&request);
        self.request = request;
        result
    }

    fn exchange_bytes(&mut self, request: &[u8]) -> Result<(), T::Error> {
        let chunks = request.len().div_ceil(self.mss);
        if chunks > 256 {
            return Err(Error::Invalid("eUICC request needs more than 256 segments"));
        }
        self.payload.clear();
        for index in 0..chunks {
            let start = index * self.mss;
            let end = core::cmp::min(start + self.mss, request.len());
            self.send_e2_chunk(index as u8, index + 1 == chunks, &request[start..end])?;
        }
        Ok(())
    }

    fn send_e2_chunk(&mut self, index: u8, final_chunk: bool, data: &[u8]) -> Result<(), T::Error> {
        self.encode_data_apdu(if final_chunk { 0x91 } else { 0x11 }, index, data)?;
        let status = self.transmit_apdu()?;
        self.finish_status(status)
    }

    fn transmit_apdu(&mut self) -> Result<u16, T::Error> {
        self.raw_response.clear();
        self.transport
            .transmit(&self.apdu, &mut self.raw_response)
            .map_err(Error::Transport)?;
        let response = ApduResponse::parse(&self.raw_response).map_err(Error::Card)?;
        Self::append_payload(&mut self.payload, response.data)?;
        Ok(response.status)
    }

    fn finish_status(&mut self, status: u16) -> Result<(), T::Error> {
        if status == 0x9000 {
            return Ok(());
        }
        if proletarius_ccid::apdu::more_available((status >> 8) as u8) {
            return self.read_get_response(status as u8);
        }
        Err(Error::Card(CardError::CardStatus(status)))
    }

    fn read_get_response(&mut self, mut le: u8) -> Result<(), T::Error> {
        for _ in 0..MAX_GET_RESPONSE_ROUNDS {
            let expected = if le == 0 { 256 } else { u32::from(le) };
            self.encode_get_response(expected)?;
            let status = self.transmit_apdu()?;
            if status == 0x9000 {
                return Ok(());
            }
            if !proletarius_ccid::apdu::more_available((status >> 8) as u8) {
                return Err(Error::Card(CardError::CardStatus(status)));
            }
            le = status as u8;
        }
        Err(Error::Invalid("too many chained GET RESPONSE frames"))
    }

    fn encode_data_apdu(&mut self, p1: u8, p2: u8, data: &[u8]) -> Result<(), T::Error> {
        if data.is_empty() || data.len() > MAX_MSS {
            return Err(Error::Invalid("invalid eUICC APDU segment"));
        }
        let cla = logical_channel_cla(0x80, self.logical_channel).map_err(Error::Card)?;
        self.apdu.clear();
        self.apdu
            .extend_from_slice(&[cla, 0xe2, p1, p2, data.len() as u8]);
        self.apdu.extend_from_slice(data);
        Ok(())
    }

    fn encode_get_response(&mut self, le: u32) -> Result<(), T::Error> {
        if le == 0 || le > 256 {
            return Err(Error::Invalid("invalid GET RESPONSE length"));
        }
        let cla = logical_channel_cla(0x80, self.logical_channel).map_err(Error::Card)?;
        self.apdu.clear();
        self.apdu
            .extend_from_slice(&[cla, 0xc0, 0, 0, if le == 256 { 0 } else { le as u8 }]);
        Ok(())
    }

    fn append_payload(payload: &mut Vec<u8>, data: &[u8]) -> Result<(), T::Error> {
        let end = payload
            .len()
            .checked_add(data.len())
            .ok_or(Error::ResponseTooLarge)?;
        if end > MAX_RESPONSE_SIZE {
            return Err(Error::ResponseTooLarge);
        }
        payload.extend_from_slice(data);
        Ok(())
    }
}

fn profile_error_is_cat_busy<E>(error: &Error<E>) -> bool {
    matches!(
        error,
        Error::ProfileOperation {
            result: ProfileOperationResult::CatBusy,
            ..
        }
    )
}

fn profile_error_is_undefined<E>(error: &Error<E>) -> bool {
    matches!(
        error,
        Error::ProfileOperation {
            result: ProfileOperationResult::Undefined,
            ..
        }
    )
}

fn profile_operation_state_matches(
    operation: ProfileOperation,
    identifier: ProfileIdentifier<'_>,
    profiles: &[ProfileInfo],
) -> bool {
    let target = profiles.iter().find(|profile| match identifier {
        ProfileIdentifier::Iccid(iccid) => iccid_matches(&profile.iccid, iccid),
        ProfileIdentifier::IsdpAid(aid) => profile.isdp_aid.as_slice() == aid,
    });
    matches!(
        (operation, target.map(|profile| profile.state)),
        (ProfileOperation::Enable, Some(ProfileState::Enabled))
            | (ProfileOperation::Disable, Some(ProfileState::Disabled))
    )
}

fn profile_error_allows_identifier_fallback<E>(error: &Error<E>) -> bool {
    matches!(
        error,
        Error::ProfileOperation {
            result: ProfileOperationResult::IccidOrAidNotFound,
            ..
        }
    )
}

fn single_tlv(input: &[u8]) -> proletarius_ccid::Result<Tlv<'_>> {
    let mut iter = bertlv::parse(input)?;
    let tlv = iter.next().ok_or(CardError::Truncated)??;
    if iter.next().is_some() {
        return Err(CardError::Invalid("multiple top-level BER-TLV values"));
    }
    Ok(tlv)
}

fn child<'a>(parent: Tlv<'a>, wanted: &[u8]) -> proletarius_ccid::Result<Option<Tlv<'a>>> {
    let mut found = None;
    let children = parent.children()?;
    for item in children {
        let item = item?;
        if item.matches(wanted) {
            found = Some(item);
        }
    }
    Ok(found)
}

fn parse_euicc_info(
    input: &[u8],
    expected: EuiccInfoVersion,
) -> proletarius_ccid::Result<EuiccInfo> {
    let outer = single_tlv(input)?;
    if outer.tag.bytes != expected.tag() || !outer.tag.constructed {
        return Err(CardError::Invalid("unexpected GetEUICCInfo response tag"));
    }
    let mut info = EuiccInfo {
        version: expected.number(),
        ..EuiccInfo::default()
    };
    for item in outer.children()? {
        let item = item?;
        match item.tag.bytes {
            TAG_INFO_EXT_RESOURCE | TAG_INFO_EXT_RESOURCE_LEGACY => {
                parse_info_ext_resource(item.value, &mut info)?;
            }
            TAG_INFO_FIRMWARE => {
                if item.value.len() > MAX_EUICC_FIRMWARE_BYTES {
                    return Err(CardError::Invalid("eUICC firmware value is too large"));
                }
                info.firmware.extend_from_slice(item.value);
            }
            TAG_INFO_SAS => {
                if item.value.len() > MAX_EUICC_TEXT_BYTES {
                    return Err(CardError::Invalid("SAS accreditation value is too large"));
                }
                info.sas_accreditation_number.extend_from_slice(item.value);
            }
            TAG_INFO_CI_LIST => {
                if !item.tag.constructed {
                    return Err(CardError::Invalid(
                        "certificate key list is not constructed",
                    ));
                }
                for key in item.children()? {
                    let key = key?;
                    if info.certificate_key_ids.len() >= MAX_EUICC_CERTIFICATE_IDS {
                        return Err(CardError::Invalid("too many certificate key IDs"));
                    }
                    if key.value.len() > MAX_EUICC_CERTIFICATE_ID_BYTES {
                        return Err(CardError::Invalid("certificate key ID is too large"));
                    }
                    info.certificate_key_ids.push(key.value.to_vec());
                }
            }
            TAG_INFO_FORBIDDEN_PPR => {
                if item.value.len() > MAX_PROFILE_OWNER_BYTES {
                    return Err(CardError::Invalid(
                        "forbidden profile policy rules are too large",
                    ));
                }
                info.forbidden_profile_policy_rules
                    .extend_from_slice(item.value);
            }
            _ => {}
        }
    }
    Ok(info)
}

fn parse_info_ext_resource(input: &[u8], info: &mut EuiccInfo) -> proletarius_ccid::Result<()> {
    // extResource is encoded as context primitive 4 by some cards even
    // though its value is a sequence. Cards are seen both with a 30 wrapper
    // and with the 82 free-NVRAM item directly inside 84, so accept either
    // bounded BER shape.
    let mut saw_value = false;
    for item in bertlv::parse(input)? {
        let item = item?;
        saw_value = true;
        if item.matches(TAG_INFO_FREE_NVRAM) {
            info.free_nvram_bytes = Some(parse_unsigned_integer(item.value)?);
            continue;
        }
        if item.tag.constructed
            && let Some(free) = child(item, TAG_INFO_FREE_NVRAM)?
        {
            info.free_nvram_bytes = Some(parse_unsigned_integer(free.value)?);
        }
    }
    if !saw_value {
        return Err(CardError::Invalid("eUICC extResource is empty"));
    }
    Ok(())
}

fn parse_configured_addresses(input: &[u8]) -> proletarius_ccid::Result<ConfiguredAddresses> {
    let outer = single_tlv(input)?;
    if outer.tag.bytes != TAG_CONFIGURED_ADDRESSES || !outer.tag.constructed {
        return Err(CardError::Invalid(
            "unexpected GetEUICCConfiguredAddresses response tag",
        ));
    }
    let mut addresses = ConfiguredAddresses::default();
    if let Some(default) = child(outer, TAG_ADDRESS_DEFAULT_DP)? {
        if default.value.len() > MAX_EUICC_TEXT_BYTES {
            return Err(CardError::Invalid("default SM-DP+ address is too large"));
        }
        addresses
            .default_smdp_address
            .extend_from_slice(default.value);
    }
    if let Some(root) = child(outer, TAG_ADDRESS_ROOT_DS)? {
        if root.value.len() > MAX_EUICC_TEXT_BYTES {
            return Err(CardError::Invalid("root SM-DS address is too large"));
        }
        addresses.root_smds_address.extend_from_slice(root.value);
    }
    Ok(addresses)
}

fn parse_nickname_result(input: &[u8]) -> proletarius_ccid::Result<NicknameResult> {
    let outer = single_tlv(input)?;
    if outer.tag.bytes != TAG_SET_NICKNAME || !outer.tag.constructed {
        return Err(CardError::Invalid("unexpected SetNickname response tag"));
    }
    let result =
        child(outer, TAG_RESULT)?.ok_or(CardError::Invalid("SetNickname result missing"))?;
    Ok(NicknameResult::from_code(parse_small_integer(
        result.value,
    )?))
}

fn parse_notification_list<E>(input: &[u8], output: &mut [NotificationInfo]) -> Result<usize, E> {
    let outer = single_tlv(input).map_err(Error::Card)?;
    if outer.tag.bytes != TAG_LIST_NOTIFICATION || !outer.tag.constructed {
        return Err(Error::Invalid("unexpected ListNotification response tag"));
    }
    if let Some(error) = child(outer, TAG_NOTIFICATION_LIST_ERROR).map_err(Error::Card)? {
        return Err(Error::NotificationOperation {
            operation: NotificationOperation::List,
            result: NotificationResult::from_code(
                parse_small_integer(error.value).map_err(Error::Card)?,
            ),
        });
    }
    let container = child(outer, TAG_NOTIFICATION_LIST)
        .map_err(Error::Card)?
        .ok_or(Error::Invalid("notification list is missing"))?;
    if !container.tag.constructed {
        return Err(Error::Invalid("notification list is not constructed"));
    }
    let mut records = Vec::new();
    for item in container.children().map_err(Error::Card)? {
        let item = item.map_err(Error::Card)?;
        if item.tag.bytes != TAG_NOTIFICATION_METADATA || !item.tag.constructed {
            return Err(Error::Invalid("unexpected notification metadata tag"));
        }
        if records.len() >= MAX_NOTIFICATION_COUNT {
            return Err(Error::Invalid("notification list exceeds the record limit"));
        }
        records.push(parse_notification_metadata(item).map_err(Error::Card)?);
    }
    if records.len() > output.len() {
        return Err(Error::OutputTooSmall);
    }
    let count = records.len();
    output[..count].clone_from_slice(&records);
    Ok(count)
}

fn parse_notification_metadata(tlv: Tlv<'_>) -> proletarius_ccid::Result<NotificationInfo> {
    let sequence = child(tlv, TAG_SEQUENCE_NUMBER)?
        .ok_or(CardError::Invalid("notification sequence number missing"))?;
    let event = child(tlv, TAG_NOTIFICATION_EVENT)?
        .ok_or(CardError::Invalid("notification event missing"))?;
    let event = parse_notification_event(event.value)?;
    if event > 3 {
        return Err(CardError::Invalid("notification event is out of range"));
    }
    let address = child(tlv, TAG_NOTIFICATION_ADDRESS)?;
    let iccid = child(tlv, TAG_IDENTIFIER)?;
    if let Some(address) = address
        && address.value.len() > MAX_NOTIFICATION_ADDRESS_BYTES
    {
        return Err(CardError::Invalid("notification address is too large"));
    }
    if let Some(iccid) = iccid
        && iccid.value.len() > MAX_NOTIFICATION_ICCID_BYTES
    {
        return Err(CardError::Invalid("notification ICCID is too large"));
    }
    Ok(NotificationInfo {
        sequence_number: parse_unsigned_integer(sequence.value)?,
        event,
        address: address.map_or_else(Vec::new, |value| value.value.to_vec()),
        iccid: iccid.map_or_else(Vec::new, |value| value.value.to_vec()),
    })
}

fn parse_notification_remove_result(input: &[u8]) -> proletarius_ccid::Result<NotificationResult> {
    let outer = single_tlv(input)?;
    if outer.tag.bytes != TAG_REMOVE_NOTIFICATION || !outer.tag.constructed {
        return Err(CardError::Invalid(
            "unexpected RemoveNotification response tag",
        ));
    }
    let result =
        child(outer, TAG_RESULT)?.ok_or(CardError::Invalid("RemoveNotification result missing"))?;
    Ok(NotificationResult::from_code(parse_small_integer(
        result.value,
    )?))
}

fn parse_notification_event(value: &[u8]) -> proletarius_ccid::Result<u8> {
    if value.len() == 1 {
        return parse_small_integer(value);
    }
    // A few implementations return the search-criteria BIT STRING instead of
    // the integer event.  Accept exactly one set bit in the four event range.
    if value.len() == 2 && value[0] <= 7 {
        let bits = value[1] >> 4;
        if bits != 0 && bits & (bits - 1) == 0 {
            let event = bits.trailing_zeros() as u8;
            if event <= 3 {
                return Ok(event);
            }
        }
    }
    Err(CardError::Invalid("notification event is malformed"))
}

fn parse_unsigned_integer(value: &[u8]) -> proletarius_ccid::Result<u64> {
    if value.is_empty() || value.len() > 9 || (value.len() == 9 && value[0] != 0) {
        return Err(CardError::Invalid("integer value is out of range"));
    }
    let mut number = 0_u64;
    for &byte in value {
        number = number
            .checked_shl(8)
            .and_then(|number| number.checked_add(u64::from(byte)))
            .ok_or(CardError::LengthOverflow)?;
    }
    Ok(number)
}

fn encode_unsigned_integer(value: u64, output: &mut [u8; 9]) -> usize {
    let mut number = value;
    let mut index = output.len();
    loop {
        index -= 1;
        output[index] = number as u8;
        number >>= 8;
        if number == 0 {
            break;
        }
    }
    if output[index] & 0x80 != 0 {
        index -= 1;
        output[index] = 0;
    }
    let length = output.len() - index;
    output.copy_within(index.., 0);
    length
}

fn parse_profile_list_into<E>(input: &[u8], output: &mut [ProfileInfo]) -> Result<usize, E> {
    let outer = single_tlv(input).map_err(Error::Card)?;
    if outer.tag.bytes != TAG_PROFILE_LIST || !outer.tag.constructed {
        return Err(Error::Invalid("unexpected GetProfilesInfo response tag"));
    }
    if let Some(error) = child(outer, TAG_PROFILE_LIST_ERROR).map_err(Error::Card)? {
        let code = parse_small_integer(error.value).map_err(Error::Card)?;
        return Err(Error::ProfileList(code));
    }
    let container = child(outer, TAG_PROFILE_LIST_CONTAINER)
        .map_err(Error::Card)?
        .ok_or(Error::Invalid(
            "GetProfilesInfo response has no profile list",
        ))?;
    // Validate the complete response before touching caller-owned entries.
    // Profile operations are followed by an automatic list refresh; keeping
    // this refresh transactional prevents a short buffer or malformed card
    // record from replacing a valid list with a partially decoded one.
    let mut profile_count = 0_usize;
    let children = container.children().map_err(Error::Card)?;
    for item in children {
        let item = item.map_err(Error::Card)?;
        if item.tag.bytes != TAG_PROFILE && item.tag.bytes != TAG_PROFILE_ALT {
            return Err(Error::Invalid("unexpected profile information tag"));
        }
        if profile_count >= MAX_PROFILE_COUNT {
            return Err(Error::Invalid("profile list exceeds the record limit"));
        }
        validate_profile(item).map_err(Error::Card)?;
        profile_count += 1;
    }

    if profile_count > output.len() {
        return Err(Error::OutputTooSmall);
    }

    let children = container.children().map_err(Error::Card)?;
    for (profile, item) in output.iter_mut().zip(children) {
        let item = item.map_err(Error::Card)?;
        parse_profile_into_unchecked(item, profile).map_err(Error::Card)?;
    }
    Ok(profile_count)
}

fn validate_profile(tlv: Tlv<'_>) -> proletarius_ccid::Result<()> {
    if !tlv.tag.constructed {
        return Err(CardError::Invalid("profile information is not constructed"));
    }
    let mut child_count = 0_usize;
    let children = tlv.children()?;
    for item in children {
        let item = item?;
        child_count = child_count
            .checked_add(1)
            .ok_or(CardError::LengthOverflow)?;
        if child_count > MAX_PROFILE_CHILDREN {
            return Err(CardError::Invalid("profile has too many fields"));
        }
        match item.tag.bytes {
            TAG_IDENTIFIER => {
                if item.value.len() > MAX_PROFILE_ICCID_BYTES {
                    return Err(CardError::Invalid("profile ICCID is too large"));
                }
            }
            TAG_ISDP_AID => {
                if item.value.len() > MAX_PROFILE_AID_BYTES {
                    return Err(CardError::Invalid("profile ISD-P AID is too large"));
                }
            }
            TAG_PROFILE_STATE => {
                parse_small_integer(item.value)?;
            }
            TAG_NICKNAME => {
                if item.value.len() > MAX_PROFILE_TEXT_BYTES {
                    return Err(CardError::Invalid("profile nickname is too large"));
                }
            }
            TAG_SERVICE_PROVIDER_NAME => {
                if item.value.len() > MAX_PROFILE_TEXT_BYTES {
                    return Err(CardError::Invalid("profile provider name is too large"));
                }
            }
            TAG_PROFILE_NAME => {
                if item.value.len() > MAX_PROFILE_TEXT_BYTES {
                    return Err(CardError::Invalid("profile name is too large"));
                }
            }
            TAG_ICON_TYPE => {
                parse_small_integer(item.value)?;
            }
            TAG_ICON => {
                if item.value.len() > MAX_PROFILE_ICON_BYTES {
                    return Err(CardError::Invalid("profile icon is too large"));
                }
            }
            TAG_PROFILE_CLASS => {
                parse_small_integer(item.value)?;
            }
            TAG_NOTIFICATION_CONFIGURATION => {
                validate_profile_notification_configurations(item)?;
            }
            TAG_PROFILE_OWNER => {
                if !item.tag.constructed {
                    return Err(CardError::Invalid("profile owner is not constructed"));
                }
                for owner_field in item.children()? {
                    let owner_field = owner_field?;
                    match owner_field.tag.bytes {
                        TAG_PROFILE_OWNER_MCC_MNC => {
                            if owner_field.value.len() != 3 {
                                return Err(CardError::Invalid(
                                    "profile owner MCC/MNC has invalid length",
                                ));
                            }
                        }
                        TAG_PROFILE_OWNER_GID1 | TAG_PROFILE_OWNER_GID2
                            if owner_field.value.len() > MAX_PROFILE_OWNER_BYTES =>
                        {
                            return Err(CardError::Invalid(
                                "profile owner group identifier is too large",
                            ));
                        }
                        _ => {}
                    }
                }
            }
            TAG_PROFILE_POLICY_RULES if item.value.len() > MAX_PROFILE_OWNER_BYTES => {
                return Err(CardError::Invalid("profile policy rules are too large"));
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_profile_into_unchecked(
    tlv: Tlv<'_>,
    profile: &mut ProfileInfo,
) -> proletarius_ccid::Result<()> {
    profile.iccid.clear();
    profile.isdp_aid.clear();
    profile.state = ProfileState::default();
    profile.nickname.clear();
    profile.service_provider_name.clear();
    profile.profile_name.clear();
    profile.icon_type = 0;
    profile.icon.clear();
    profile.class = ProfileClass::default();
    profile.notification_configurations.clear();
    profile.mcc_mnc.clear();
    profile.gid1.clear();
    profile.gid2.clear();
    profile.profile_policy_rules.clear();
    for item in tlv.children()? {
        let item = item?;
        match item.tag.bytes {
            TAG_IDENTIFIER => {
                profile.iccid.clear();
                profile.iccid.extend_from_slice(item.value);
            }
            TAG_ISDP_AID => {
                profile.isdp_aid.clear();
                profile.isdp_aid.extend_from_slice(item.value);
            }
            TAG_PROFILE_STATE => profile.state = profile_state(parse_small_integer(item.value)?),
            TAG_NICKNAME => {
                profile.nickname.clear();
                profile.nickname.extend_from_slice(item.value);
            }
            TAG_SERVICE_PROVIDER_NAME => {
                profile.service_provider_name.clear();
                profile.service_provider_name.extend_from_slice(item.value)
            }
            TAG_PROFILE_NAME => {
                profile.profile_name.clear();
                profile.profile_name.extend_from_slice(item.value);
            }
            TAG_ICON_TYPE => profile.icon_type = parse_small_integer(item.value)?,
            TAG_ICON => {
                profile.icon.clear();
                profile.icon.extend_from_slice(item.value);
            }
            TAG_PROFILE_CLASS => profile.class = profile_class(parse_small_integer(item.value)?),
            TAG_NOTIFICATION_CONFIGURATION => {
                parse_profile_notification_configurations(
                    item,
                    &mut profile.notification_configurations,
                )?;
            }
            TAG_PROFILE_OWNER => {
                for owner_field in item.children()? {
                    let owner_field = owner_field?;
                    match owner_field.tag.bytes {
                        TAG_PROFILE_OWNER_MCC_MNC => {
                            profile.mcc_mnc.extend_from_slice(owner_field.value);
                        }
                        TAG_PROFILE_OWNER_GID1 => {
                            profile.gid1.extend_from_slice(owner_field.value);
                        }
                        TAG_PROFILE_OWNER_GID2 => {
                            profile.gid2.extend_from_slice(owner_field.value);
                        }
                        _ => {}
                    }
                }
            }
            TAG_PROFILE_POLICY_RULES => {
                profile.profile_policy_rules.extend_from_slice(item.value);
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_profile_notification_configurations(tlv: Tlv<'_>) -> proletarius_ccid::Result<()> {
    if !tlv.tag.constructed {
        return Err(CardError::Invalid(
            "profile notification configuration is not constructed",
        ));
    }
    let mut count = 0_usize;
    for entry in tlv.children()? {
        let entry = entry?;
        count = count.checked_add(1).ok_or(CardError::LengthOverflow)?;
        if count > MAX_PROFILE_NOTIFICATION_CONFIGURATIONS {
            return Err(CardError::Invalid(
                "profile has too many notification configurations",
            ));
        }
        if entry.tag.bytes != TAG_NOTIFICATION_CONFIGURATION_ENTRY || !entry.tag.constructed {
            return Err(CardError::Invalid(
                "profile notification configuration entry is invalid",
            ));
        }
        for field in entry.children()? {
            let field = field?;
            match field.tag.bytes {
                TAG_NOTIFICATION_CONFIGURATION_EVENT
                    if field.value.len() > MAX_PROFILE_NOTIFICATION_EVENT_BYTES =>
                {
                    return Err(CardError::Invalid(
                        "profile notification event mask is too large",
                    ));
                }
                TAG_NOTIFICATION_CONFIGURATION_ADDRESS
                    if field.value.len() > MAX_PROFILE_NOTIFICATION_ADDRESS_BYTES
                        || core::str::from_utf8(field.value).is_err() =>
                {
                    return Err(CardError::Invalid(
                        "profile notification address is invalid",
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn parse_profile_notification_configurations(
    tlv: Tlv<'_>,
    output: &mut Vec<ProfileNotificationConfiguration>,
) -> proletarius_ccid::Result<()> {
    for entry in tlv.children()? {
        let entry = entry?;
        let mut configuration = ProfileNotificationConfiguration::default();
        for field in entry.children()? {
            let field = field?;
            match field.tag.bytes {
                TAG_NOTIFICATION_CONFIGURATION_EVENT => {
                    configuration.events.extend_from_slice(field.value);
                }
                TAG_NOTIFICATION_CONFIGURATION_ADDRESS => {
                    configuration.address.extend_from_slice(field.value);
                }
                _ => {}
            }
        }
        output.push(configuration);
    }
    Ok(())
}

fn parse_operation_result(
    input: &[u8],
    operation: ProfileOperation,
) -> proletarius_ccid::Result<ProfileOperationResult> {
    let outer = single_tlv(input)?;
    if outer.tag.bytes != operation.tag() || !outer.tag.constructed {
        return Err(CardError::Invalid(
            "unexpected profile operation response tag",
        ));
    }
    let result =
        child(outer, TAG_RESULT)?.ok_or(CardError::Invalid("profile operation result missing"))?;
    Ok(ProfileOperationResult::from_code(parse_small_integer(
        result.value,
    )?))
}

fn parse_small_integer(value: &[u8]) -> proletarius_ccid::Result<u8> {
    if value.is_empty() || value.len() > 2 {
        return Err(CardError::Invalid("integer value is out of range"));
    }
    let mut number = 0_u16;
    for &byte in value {
        number = number
            .checked_shl(8)
            .and_then(|number| number.checked_add(u16::from(byte)))
            .ok_or(CardError::LengthOverflow)?;
    }
    u8::try_from(number).map_err(|_| CardError::Invalid("integer value is out of range"))
}

const fn profile_state(value: u8) -> ProfileState {
    match value {
        0 => ProfileState::Disabled,
        1 => ProfileState::Enabled,
        other => ProfileState::Unknown(other),
    }
}

const fn profile_class(value: u8) -> ProfileClass {
    match value {
        0 => ProfileClass::Test,
        1 => ProfileClass::Provisioning,
        2 => ProfileClass::Operational,
        other => ProfileClass::Unknown(other),
    }
}

/// Encode decimal ICCID digits into the GSM-swapped BCD representation used
/// inside the `5A` eUICC profile identifier.
pub fn encode_iccid(digits: &str) -> core::result::Result<Vec<u8>, IccidError> {
    if digits.is_empty() || digits.len() > 32 {
        return Err(IccidError::InvalidLength);
    }
    let mut output = Vec::with_capacity(digits.len().div_ceil(2));
    let bytes = digits.as_bytes();
    for pair in bytes.chunks(2) {
        let low = nibble(pair[0]).ok_or(IccidError::NonDecimal)?;
        let high = pair
            .get(1)
            .map_or(Some(0x0f), |byte| nibble(*byte))
            .ok_or(IccidError::NonDecimal)?;
        output.push(low | (high << 4));
    }
    Ok(output)
}

/// Decode a GSM-swapped BCD ICCID into decimal digits.
pub fn decode_iccid(raw: &[u8]) -> core::result::Result<String, IccidError> {
    decode_swapped_bcd(raw)
}

fn decode_swapped_bcd(raw: &[u8]) -> core::result::Result<String, IccidError> {
    if raw.is_empty() || raw.len() > 16 {
        return Err(IccidError::InvalidLength);
    }
    let mut output = String::with_capacity(raw.len() * 2);
    let mut padding = false;
    for &byte in raw {
        for digit in [byte & 0x0f, byte >> 4] {
            match digit {
                0..=9 if !padding => output.push(char::from(b'0' + digit)),
                0x0f => padding = true,
                _ => return Err(IccidError::InvalidBcd),
            }
        }
    }
    if output.is_empty() {
        Err(IccidError::InvalidBcd)
    } else {
        Ok(output)
    }
}

/// Compare two ICCID encodings after decoding their swapped-BCD payloads.
/// Cards are allowed to retain fixed-width trailing `F` nibbles, while
/// callers normally submit the shortest decimal representation.
pub fn iccid_matches(left: &[u8], right: &[u8]) -> bool {
    match (decode_swapped_bcd(left), decode_swapped_bcd(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn nibble(byte: u8) -> Option<u8> {
    byte.checked_sub(b'0').filter(|value| *value <= 9)
}

#[cfg(test)]
mod notification_list_tests {
    use super::*;
    use alloc::vec;

    fn response(body: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        bertlv::encode_constructed(TAG_LIST_NOTIFICATION, body, &mut output).unwrap();
        output
    }

    fn list(records: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        bertlv::encode_constructed(TAG_NOTIFICATION_LIST, records, &mut output).unwrap();
        response(&output)
    }

    fn existing() -> NotificationInfo {
        NotificationInfo {
            sequence_number: 91,
            event: 3,
            address: b"existing.example".to_vec(),
            iccid: vec![0x89, 0x01],
        }
    }

    fn metadata(sequence: u8) -> Vec<u8> {
        let mut value = Vec::new();
        bertlv::encode(TAG_SEQUENCE_NUMBER, &[sequence], &mut value).unwrap();
        bertlv::encode(TAG_NOTIFICATION_EVENT, &[4, 0x80], &mut value).unwrap();
        let mut output = Vec::new();
        bertlv::encode_constructed(TAG_NOTIFICATION_METADATA, &value, &mut output).unwrap();
        output
    }

    #[test]
    fn list_error_choice_is_not_a_successful_empty_list() {
        for (body, expected) in [
            (vec![0x81, 1, 127], NotificationResult::Undefined),
            (vec![0x81, 1, 42], NotificationResult::Unknown(42)),
            (vec![0xa0, 0, 0x81, 1, 127], NotificationResult::Undefined),
        ] {
            let mut output = [existing()];
            assert_eq!(
                parse_notification_list::<()>(&response(&body), &mut output),
                Err(Error::NotificationOperation {
                    operation: NotificationOperation::List,
                    result: expected,
                })
            );
            assert_eq!(output, [existing()]);
        }
    }

    #[test]
    fn malformed_list_error_or_missing_container_preserves_output() {
        for body in [&[][..], &[0x81, 0], &[0x81, 2, 1, 0], &[0x80, 0]] {
            let mut output = [existing()];
            assert!(parse_notification_list::<()>(&response(body), &mut output).is_err());
            assert_eq!(output, [existing()]);
        }
    }

    #[test]
    fn empty_notification_container_remains_a_valid_empty_list() {
        let mut output = [existing()];
        assert_eq!(
            parse_notification_list::<()>(&list(&[]), &mut output),
            Ok(0)
        );
        assert_eq!(output, [existing()]);
        assert_eq!(parse_notification_list::<()>(&list(&[]), &mut []), Ok(0));
    }

    #[test]
    fn malformed_later_metadata_does_not_replace_any_output() {
        let mut records = metadata(7);
        // A valid first record followed by a truncated metadata value.
        records.extend_from_slice(&[0xbf, 0x2f, 0x02, 0x80]);
        let mut output = [existing(), existing()];
        assert!(parse_notification_list::<()>(&list(&records), &mut output).is_err());
        assert_eq!(output, [existing(), existing()]);
    }

    #[test]
    fn notification_list_replaces_only_complete_records_that_fit() {
        let mut records = metadata(7);
        records.extend(metadata(8));
        let mut short = [existing()];
        assert_eq!(
            parse_notification_list::<()>(&list(&records), &mut short),
            Err(Error::OutputTooSmall)
        );
        assert_eq!(short, [existing()]);
        let mut output = [existing(), existing(), existing()];
        assert_eq!(
            parse_notification_list::<()>(&list(&records), &mut output),
            Ok(2)
        );
        assert_eq!(output[0].sequence_number, 7);
        assert_eq!(output[1].sequence_number, 8);
        assert_eq!(output[2], existing());
    }
}

#[cfg(test)]
mod b0_apdu_tests {
    use super::*;
    use alloc::vec::Vec;
    use core::convert::Infallible;

    struct Capture(Vec<Vec<u8>>);

    impl CardTransport for Capture {
        type Error = Infallible;

        fn transmit(
            &mut self,
            command: &[u8],
            response: &mut Vec<u8>,
        ) -> core::result::Result<(), Self::Error> {
            self.0.push(command.to_vec());
            response.clear();
            response.extend_from_slice(&[0x90, 0x00]);
            Ok(())
        }
    }

    #[test]
    fn b0_auth_uses_selected_logical_channel_and_exact_bf71_tlv() {
        let mut euicc = Euicc::new(Capture(Vec::new()), 1, DEFAULT_MSS).unwrap();
        let body: Vec<u8> = (0..27).collect();
        assert_eq!(
            euicc.transmit_b0_authentication(&body).unwrap(),
            [0x90, 0x00]
        );
        let transport = euicc.into_inner();
        assert_eq!(transport.0[0][..5], [0x81, 0xe2, 0x91, 0x00, 27]);
        assert_eq!(&transport.0[0][5..], body);
    }

    #[test]
    fn b0_auth_rejects_wrong_tlv_length_before_transport() {
        let mut euicc = Euicc::new(Capture(Vec::new()), 1, DEFAULT_MSS).unwrap();
        assert!(euicc.transmit_b0_authentication(&[0; 24]).is_err());
        assert!(euicc.into_inner().0.is_empty());
    }
}

#[cfg(test)]
mod profile_command_tests;
