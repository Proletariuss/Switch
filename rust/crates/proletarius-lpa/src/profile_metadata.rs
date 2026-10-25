// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! Minimal, bounded parser for GSMA profile metadata and the download-size
//! fallback used by the existing Switch confirmation flow.

use std::sync::OnceLock;

use proletarius_ccid::bertlv::{self, Tlv};
use serde::Deserialize;

const PROFILE_SIZE_REFERENCES: &[u8] = include_bytes!("../resources/reference_sizes_simple.json");

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProfileMetadata {
    pub estimated_profile_size_bytes: Option<i64>,
    pub service_provider_name: Option<String>,
    pub plmn: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileMetadataError {
    TooLarge,
    InvalidBerTlv,
    UnexpectedRootTag,
    InvalidEstimatedSize,
    InvalidOwner,
    InvalidProfileField,
    InvalidNotification,
}

impl std::fmt::Display for ProfileMetadataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "profile metadata exceeds the BER-TLV size limit",
            Self::InvalidBerTlv => "profile metadata is not valid BER-TLV",
            Self::UnexpectedRootTag => "profile metadata has an unexpected root tag",
            Self::InvalidEstimatedSize => "profile metadata has an invalid estimated size",
            Self::InvalidOwner => "profile metadata has an invalid profile owner",
            Self::InvalidProfileField => "profile metadata has an invalid profile field",
            Self::InvalidNotification => {
                "profile metadata has an invalid notification configuration"
            }
        })
    }
}

impl std::error::Error for ProfileMetadataError {}

#[derive(Deserialize)]
struct ReferenceDatabase {
    reference_eum: String,
    results: Vec<ReferenceItem>,
}

#[derive(Deserialize)]
struct ReferenceItem {
    plmn: String,
    #[serde(rename = "serviceProviderName")]
    service_provider_name: String,
    rsp: String,
    reference_size: i64,
    eum_sizes: std::collections::HashMap<String, i64>,
}

static REFERENCE_DATABASE: OnceLock<Option<ReferenceDatabase>> = OnceLock::new();

/// Parse the profile fields used to confirm a download. Unknown metadata tags
/// are BER-TLV validated and then ignored, as in the Go implementation.
pub fn parse_profile_metadata(input: &[u8]) -> Result<ProfileMetadata, ProfileMetadataError> {
    if input.len() > bertlv::MAX_TLV_SIZE {
        return Err(ProfileMetadataError::TooLarge);
    }
    let mut roots = bertlv::parse(input).map_err(|_| ProfileMetadataError::InvalidBerTlv)?;
    let root = roots
        .next()
        .transpose()
        .map_err(|_| ProfileMetadataError::InvalidBerTlv)?
        .ok_or(ProfileMetadataError::InvalidBerTlv)?;
    if roots.next().is_some() {
        return Err(ProfileMetadataError::InvalidBerTlv);
    }
    if !root.tag.constructed || !(root.matches(&[0xE3]) || root.matches(&[0xBF, 0x25])) {
        return Err(ProfileMetadataError::UnexpectedRootTag);
    }
    validate_nested_tlvs(root, 0)?;

    let mut metadata = ProfileMetadata::default();
    let mut service_provider_seen = false;
    let mut estimated_size_seen = false;
    let mut owner_seen = false;
    let mut profile_state_seen = false;
    let mut icon_type_seen = false;
    let mut profile_class_seen = false;
    let mut notification_seen = false;
    let mut policy_rules_seen = false;
    let children = root
        .children()
        .map_err(|_| ProfileMetadataError::InvalidBerTlv)?;
    for child in children {
        let child = child.map_err(|_| ProfileMetadataError::InvalidBerTlv)?;
        match child.tag.bytes {
            [0x91] if !service_provider_seen => {
                service_provider_seen = true;
                metadata.service_provider_name =
                    Some(String::from_utf8_lossy(child.value).into_owned());
            }
            [0x9F, 0x21] if !estimated_size_seen => {
                estimated_size_seen = true;
                metadata.estimated_profile_size_bytes = Some(
                    parse_signed_integer(child.value)
                        .ok_or(ProfileMetadataError::InvalidEstimatedSize)?,
                );
            }
            [0xB7] if child.tag.constructed && !owner_seen => {
                owner_seen = true;
                metadata.plmn = parse_owner(child)?;
            }
            [0x9F, 0x51] if !profile_state_seen => {
                profile_state_seen = true;
                parse_signed_integer_with_width(child.value, 1)
                    .ok_or(ProfileMetadataError::InvalidProfileField)?;
            }
            [0x93] if !icon_type_seen => {
                icon_type_seen = true;
                parse_signed_integer_with_width(child.value, 1)
                    .ok_or(ProfileMetadataError::InvalidProfileField)?;
            }
            [0x95] if !profile_class_seen => {
                profile_class_seen = true;
                parse_signed_integer_with_width(child.value, 1)
                    .ok_or(ProfileMetadataError::InvalidProfileField)?;
            }
            [0xB6] if child.tag.constructed && !notification_seen => {
                notification_seen = true;
                validate_notifications(child)?;
            }
            [0x99] if !policy_rules_seen => {
                policy_rules_seen = true;
                validate_bit_string(child.value)
                    .ok_or(ProfileMetadataError::InvalidProfileField)?;
            }
            _ => {}
        }
    }
    Ok(metadata)
}

fn parse_owner(owner: Tlv<'_>) -> Result<Option<String>, ProfileMetadataError> {
    let children = owner
        .children()
        .map_err(|_| ProfileMetadataError::InvalidOwner)?;
    for child in children {
        let child = child.map_err(|_| ProfileMetadataError::InvalidOwner)?;
        if child.matches(&[0x80]) {
            return Ok(decode_plmn(child.value));
        }
    }
    Ok(None)
}

fn validate_nested_tlvs(tlv: Tlv<'_>, depth: usize) -> Result<(), ProfileMetadataError> {
    if !tlv.tag.constructed {
        return Ok(());
    }
    if depth >= 64 {
        return Err(ProfileMetadataError::InvalidBerTlv);
    }
    for child in tlv
        .children()
        .map_err(|_| ProfileMetadataError::InvalidBerTlv)?
    {
        validate_nested_tlvs(
            child.map_err(|_| ProfileMetadataError::InvalidBerTlv)?,
            depth + 1,
        )?;
    }
    Ok(())
}

fn validate_notifications(notifications: Tlv<'_>) -> Result<(), ProfileMetadataError> {
    for child in notifications
        .children()
        .map_err(|_| ProfileMetadataError::InvalidNotification)?
    {
        let sequence = child.map_err(|_| ProfileMetadataError::InvalidNotification)?;
        if !sequence.tag.constructed || !sequence.matches(&[0x30]) {
            return Err(ProfileMetadataError::InvalidNotification);
        }
        let mut operations = None;
        let mut address_present = false;
        for field in sequence
            .children()
            .map_err(|_| ProfileMetadataError::InvalidNotification)?
        {
            let field = field.map_err(|_| ProfileMetadataError::InvalidNotification)?;
            match field.tag.bytes {
                [0x80] if operations.is_none() => operations = Some(field.value),
                [0x81] if !address_present => address_present = true,
                _ => {}
            }
        }
        let operation_bits = operations.ok_or(ProfileMetadataError::InvalidNotification)?;
        let bits =
            validate_bit_string(operation_bits).ok_or(ProfileMetadataError::InvalidNotification)?;
        if !bits.iter().any(|bit| *bit)
            || bits
                .iter()
                .enumerate()
                .any(|(index, bit)| *bit && index > 3)
            || !address_present
        {
            return Err(ProfileMetadataError::InvalidNotification);
        }
    }
    Ok(())
}

fn validate_bit_string(data: &[u8]) -> Option<Vec<bool>> {
    let (&padding, bytes) = data.split_first()?;
    if padding > 7
        || bytes.is_empty() && padding > 0
        || bytes
            .last()
            .is_some_and(|last| last & ((1_u8 << padding) - 1) != 0)
    {
        return None;
    }
    let bit_count = bytes
        .len()
        .checked_mul(8)?
        .checked_sub(usize::from(padding))?;
    let mut bits = Vec::with_capacity(bit_count);
    for index in 0..bit_count {
        bits.push(bytes[index / 8] & (1 << (7 - index % 8)) != 0);
    }
    Some(bits)
}

fn decode_plmn(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 3 {
        return None;
    }
    let digits = [
        bytes[0] & 0x0f,
        bytes[0] >> 4,
        bytes[1] & 0x0f,
        bytes[2] & 0x0f,
        bytes[2] >> 4,
    ];
    let mut output = String::with_capacity(6);
    for digit in digits {
        if digit > 9 {
            return None;
        }
        output.push(char::from(b'0' + digit));
    }
    let final_digit = bytes[1] >> 4;
    if final_digit != 0x0f {
        if final_digit > 9 {
            return None;
        }
        output.push(char::from(b'0' + final_digit));
    }
    Some(output)
}

fn parse_signed_integer(bytes: &[u8]) -> Option<i64> {
    parse_signed_integer_with_width(bytes, 8)
}

fn parse_signed_integer_with_width(bytes: &[u8], width: usize) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let mut bytes = bytes;
    while bytes.len() > 1 {
        let first = bytes[0];
        let second = bytes[1];
        if (first == 0x00 && second & 0x80 == 0) || (first == 0xff && second & 0x80 != 0) {
            bytes = &bytes[1..];
        } else {
            break;
        }
    }
    if bytes.len() > width {
        return None;
    }
    let mut value = if bytes[0] & 0x80 == 0 {
        0_u64
    } else {
        u64::MAX
    };
    for byte in bytes {
        value = (value << 8) | u64::from(*byte);
    }
    Some(value as i64)
}

/// Estimate the profile size in bytes. A positive SM-DP+ estimate always
/// wins; the shared reference table is consulted only when it is absent.
#[must_use]
pub fn estimate_profile_size(smdp: &str, eid: &str, metadata: &ProfileMetadata) -> i64 {
    if let Some(size) = metadata
        .estimated_profile_size_bytes
        .filter(|size| *size > 0)
    {
        return size;
    }
    let database = REFERENCE_DATABASE
        .get_or_init(|| serde_json::from_slice(PROFILE_SIZE_REFERENCES).ok())
        .as_ref();
    let Some(database) = database else {
        return 0;
    };

    let smdp = normalize_rsp(smdp);
    let provider = metadata
        .service_provider_name
        .as_deref()
        .unwrap_or_default()
        .trim();
    let plmn = metadata.plmn.as_deref().unwrap_or_default();
    let mut best: Option<(&ReferenceItem, i32)> = None;
    for item in &database.results {
        if smdp.is_empty() || normalize_rsp(&item.rsp) != smdp {
            continue;
        }
        let mut score = 1;
        if !plmn.is_empty() && item.plmn == plmn {
            score += 2;
        }
        let expected = item.service_provider_name.trim();
        if !provider.is_empty()
            && !expected.is_empty()
            && provider.to_lowercase().contains(&expected.to_lowercase())
        {
            score += 3;
        }
        if best.is_none_or(|(_, current_score)| score > current_score) {
            best = Some((item, score));
        }
    }
    let Some((item, _)) = best.filter(|(item, _)| item.reference_size > 0) else {
        return 0;
    };

    let eid_prefix = eid.trim().get(..8).unwrap_or_default();
    if !eid_prefix.is_empty() {
        if let Some(size) = item.eum_sizes.get(eid_prefix).filter(|size| **size > 0) {
            return *size;
        }
        if eid_prefix.eq_ignore_ascii_case(&database.reference_eum) {
            return item.reference_size;
        }
    }
    item.reference_size
}

fn normalize_rsp(value: &str) -> String {
    let mut value = value.trim().to_lowercase();
    if let Some(rest) = value.strip_prefix("https://") {
        value = String::from(rest);
    }
    if let Some(rest) = value.strip_prefix("http://") {
        value = String::from(rest);
    }
    value.trim_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    const COSMOTE_METADATA: &[u8] = &[
        0xE3, 0x10, 0x91, 0x07, b'C', b'O', b'S', b'M', b'O', b'T', b'E', 0xB7, 0x05, 0x80, 0x03,
        0x02, 0xF2, 0x10,
    ];

    #[test]
    fn parses_go_profile_metadata_fixture_fields() {
        let metadata = parse_profile_metadata(COSMOTE_METADATA).unwrap();
        assert_eq!(metadata.service_provider_name.as_deref(), Some("COSMOTE"));
        assert_eq!(metadata.plmn.as_deref(), Some("20201"));
        assert_eq!(metadata.estimated_profile_size_bytes, None);
    }

    #[test]
    fn parses_and_validates_the_go_authenticate_client_fixture() {
        let input = base64::engine::general_purpose::STANDARD
            .decode("vyWBjVoKmFgyJCBCSCZpZJEGQ01MSU5LkgdDTUlfR0RTthowGIACBHCBEmNvbnN1bWVyLnJzcC53b3JsZLcdgANU9CGBCv////////////+CCv////////////+/djLiMOEiwSA6yVumdHCV8I0+WJoSqtB4vuLOqh4/PnGVvchLJYeB2OMK2wgAAAAAAAAAAQ==")
            .unwrap();
        let metadata = parse_profile_metadata(&input).unwrap();
        assert_eq!(metadata.service_provider_name.as_deref(), Some("CMLINK"));
    }

    #[test]
    fn smdp_estimated_size_precedes_reference_fallback() {
        let metadata = ProfileMetadata {
            estimated_profile_size_bytes: Some(123_456),
            service_provider_name: None,
            plmn: None,
        };
        assert_eq!(
            estimate_profile_size("unknown.example", "", &metadata),
            123_456
        );
    }

    #[test]
    fn uses_nekoko_reference_size_for_matching_rsp_provider_plmn_and_eum() {
        let metadata = parse_profile_metadata(COSMOTE_METADATA).unwrap();
        assert_eq!(
            estimate_profile_size(
                "https://rsp-0003.oberthur.net/",
                "89044045930000000000001714825038",
                &metadata,
            ),
            39_008
        );
    }

    #[test]
    fn does_not_guess_size_for_unknown_rsp() {
        let metadata = ProfileMetadata {
            service_provider_name: Some(String::from("Unknown")),
            ..ProfileMetadata::default()
        };
        assert_eq!(estimate_profile_size("unlisted.example", "", &metadata), 0);
    }

    #[test]
    fn parses_optional_signed_estimated_profile_size() {
        let mut data = vec![0xE3, 0x0F, 0x9F, 0x21, 0x03, 0x01, 0x23, 0x45];
        data.splice(2..2, [0x91, 0x07, b'C', b'O', b'S', b'M', b'O', b'T', b'E']);
        let metadata = parse_profile_metadata(&data).unwrap();
        assert_eq!(metadata.estimated_profile_size_bytes, Some(0x012345));
    }

    #[test]
    fn accepts_context_specific_profile_metadata_root() {
        let metadata = parse_profile_metadata(&[0xBF, 0x25, 0x04, 0x9F, 0x21, 0x01, 0x7F]).unwrap();
        assert_eq!(metadata.estimated_profile_size_bytes, Some(127));
    }

    #[test]
    fn rejects_overflowing_profile_state_and_invalid_policy_bit_string() {
        assert_eq!(
            parse_profile_metadata(&[0xE3, 0x05, 0x9F, 0x51, 0x02, 0x01, 0x00]),
            Err(ProfileMetadataError::InvalidProfileField)
        );
        assert_eq!(
            parse_profile_metadata(&[0xE3, 0x03, 0x99, 0x01, 0x08]),
            Err(ProfileMetadataError::InvalidProfileField)
        );
    }

    #[test]
    fn rejects_notification_events_outside_go_supported_range() {
        assert_eq!(
            parse_profile_metadata(&[
                0xE3, 0x0B, 0xB6, 0x09, 0x30, 0x07, 0x80, 0x02, 0x00, 0x08, 0x81, 0x01, b'x',
            ]),
            Err(ProfileMetadataError::InvalidNotification)
        );
    }

    #[test]
    fn rejects_malformed_metadata_and_multiple_roots() {
        assert_eq!(
            parse_profile_metadata(&[0xE3, 0x04, 0x91, 0x01]),
            Err(ProfileMetadataError::InvalidBerTlv)
        );
        assert_eq!(
            parse_profile_metadata(&[0xE3, 0x00, 0xE3, 0x00]),
            Err(ProfileMetadataError::InvalidBerTlv)
        );
    }
}
