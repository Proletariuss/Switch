// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! ES10b Bound Profile Package validation and card loading.
//!
//! A package can be several megabytes, so this module never copies it into the
//! eUICC session buffers. It validates the complete BER structure first, then
//! borrows contiguous package ranges while sending the required ES10b command
//! sequence. Network authentication and SM-DP+ notification delivery belong
//! to `celmux-lpa`, not to the card transport.

use proletarius_ccid::{Error as CardError, bertlv::Tlv};

use super::{
    CardTransport, Error, Euicc, MAX_PROFILE_AID_BYTES, ProfileInfo, Result, parse_small_integer,
    single_tlv,
};

/// Upper bound accepted for one encoded Bound Profile Package.
pub const MAX_BOUND_PROFILE_PACKAGE_SIZE: usize = 16 * 1024 * 1024;
const MAX_BOUND_PROFILE_SEGMENTS: usize = 65_536;
const MAX_TRANSACTION_ID_BYTES: usize = 64;

const TAG_BOUND_PROFILE_PACKAGE: &[u8] = &[0xbf, 0x36];
const TAG_INITIALISE_SECURE_CHANNEL: &[u8] = &[0xbf, 0x23];
const TAG_FIRST_SEQUENCE_OF_87: &[u8] = &[0xa0];
const TAG_SEQUENCE_OF_88: &[u8] = &[0xa1];
const TAG_SECOND_SEQUENCE_OF_87: &[u8] = &[0xa2];
const TAG_SEQUENCE_OF_86: &[u8] = &[0xa3];
const TAG_PROFILE_ELEMENT_87: &[u8] = &[0x87];
const TAG_PROFILE_ELEMENT_88: &[u8] = &[0x88];
const TAG_PROFILE_ELEMENT_86: &[u8] = &[0x86];

const TAG_LOAD_RESPONSE: &[u8] = &[0xbf, 0x37];
const TAG_PROFILE_INSTALLATION_RESULT: &[u8] = &[0xbf, 0x27];
const TAG_TRANSACTION_ID: &[u8] = &[0x80];
const TAG_NOTIFICATION_METADATA: &[u8] = &[0xbf, 0x2f];
const TAG_FINAL_RESULT: &[u8] = &[0xa2];
const TAG_SUCCESS_RESULT: &[u8] = &[0xa0];
const TAG_ERROR_RESULT: &[u8] = &[0xa1];
const TAG_COMMAND_ID: &[u8] = &[0x80];
const TAG_ERROR_REASON: &[u8] = &[0x81];
const TAG_ISDP_AID: &[u8] = &[0x4f];

/// Borrowed result returned by `ES10b.LoadBoundProfilePackage`.
///
/// The view remains valid until the next request on the same [`Euicc`]. The
/// raw response is retained because the LPA network layer must later forward
/// the contained notification to the SM-DP+ server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileInstallResponse<'a> {
    raw: &'a [u8],
    transaction_id: &'a [u8],
    notification: Option<&'a [u8]>,
    isdp_aid: &'a [u8],
}

impl<'a> ProfileInstallResponse<'a> {
    #[must_use]
    pub const fn raw(&self) -> &'a [u8] {
        self.raw
    }

    #[must_use]
    pub const fn transaction_id(&self) -> &'a [u8] {
        self.transaction_id
    }

    /// Value bytes of the optional notification metadata TLV.
    #[must_use]
    pub const fn notification(&self) -> Option<&'a [u8]> {
        self.notification
    }

    #[must_use]
    pub const fn isdp_aid(&self) -> &'a [u8] {
        self.isdp_aid
    }
}

/// Fixed-size outcome for callers that install and immediately refresh the
/// profile list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileInstallOutcome {
    profile_count: usize,
    isdp_aid: [u8; MAX_PROFILE_AID_BYTES],
    isdp_aid_length: u8,
}

impl ProfileInstallOutcome {
    #[must_use]
    pub const fn profile_count(&self) -> usize {
        self.profile_count
    }

    #[must_use]
    pub fn isdp_aid(&self) -> &[u8] {
        &self.isdp_aid[..usize::from(self.isdp_aid_length)]
    }
}

impl<T: CardTransport> Euicc<T> {
    /// Validate and load an encoded Bound Profile Package without copying it.
    ///
    /// Each ES10b command is further split according to the configured APDU
    /// MSS. The returned response borrows the session's receive buffer.
    pub fn load_bound_profile_package<'a>(
        &'a mut self,
        package: &[u8],
    ) -> Result<ProfileInstallResponse<'a>, T::Error> {
        let layout = BoundProfileLayout::parse(package).map_err(Error::Invalid)?;
        if !self.send_profile_package(layout)? {
            return Err(Error::Invalid(
                "LoadBoundProfilePackage returned no final response",
            ));
        }
        parse_install_response(&self.payload)
    }

    /// Load a profile and then replace the caller-owned profile snapshot.
    ///
    /// The card installation is complete before the list request is issued.
    /// If the refresh fails, the transactional list parser leaves every
    /// existing output entry unchanged and the caller can retry the refresh.
    pub fn install_bound_profile_package_into(
        &mut self,
        package: &[u8],
        profiles: &mut [ProfileInfo],
    ) -> Result<ProfileInstallOutcome, T::Error> {
        let mut isdp_aid = [0_u8; MAX_PROFILE_AID_BYTES];
        let isdp_aid_length = {
            let response = self.load_bound_profile_package(package)?;
            let aid = response.isdp_aid();
            isdp_aid[..aid.len()].copy_from_slice(aid);
            aid.len() as u8
        };
        let profile_count = self.list_profiles_into(profiles)?;
        Ok(ProfileInstallOutcome {
            profile_count,
            isdp_aid,
            isdp_aid_length,
        })
    }

    fn send_profile_package(&mut self, layout: BoundProfileLayout<'_>) -> Result<bool, T::Error> {
        let initial_end = layout
            .outer
            .header_length
            .checked_add(layout.initial.encoded.len())
            .ok_or(Error::Invalid("Bound Profile Package length overflow"))?;
        if self.send_profile_segment(&layout.outer.encoded[..initial_end])? {
            return Ok(true);
        }
        if self.send_profile_sequence(layout.first_87, true)? {
            return Ok(true);
        }
        if self.send_profile_sequence(layout.sequence_88, false)? {
            return Ok(true);
        }
        if let Some(second_87) = layout.second_87
            && self.send_profile_sequence(second_87, true)?
        {
            return Ok(true);
        }
        self.send_profile_sequence(layout.sequence_86, false)
    }

    fn send_profile_sequence(
        &mut self,
        sequence: LargeTlv<'_>,
        combine_first_child: bool,
    ) -> Result<bool, T::Error> {
        let mut children = sequence.children().map_err(Error::Invalid)?;
        if combine_first_child {
            let first = children
                .next()
                .ok_or(Error::Invalid("Bound Profile Package sequence is empty"))?
                .map_err(Error::Invalid)?;
            let end = sequence
                .header_length
                .checked_add(first.encoded.len())
                .ok_or(Error::Invalid("Bound Profile Package length overflow"))?;
            if self.send_profile_segment(&sequence.encoded[..end])? {
                return Ok(true);
            }
        } else if self.send_profile_segment(&sequence.encoded[..sequence.header_length])? {
            return Ok(true);
        }
        for child in children {
            let child = child.map_err(Error::Invalid)?;
            if self.send_profile_segment(child.encoded)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn send_profile_segment(&mut self, segment: &[u8]) -> Result<bool, T::Error> {
        self.exchange_bytes(segment)?;
        Ok(!self.payload.is_empty())
    }
}

fn parse_install_response<E>(input: &[u8]) -> Result<ProfileInstallResponse<'_>, E> {
    let outer = single_tlv(input).map_err(Error::Card)?;
    if outer.tag.bytes != TAG_LOAD_RESPONSE || !outer.tag.constructed {
        return Err(Error::Invalid(
            "unexpected LoadBoundProfilePackage response tag",
        ));
    }
    let result =
        required_unique_child(outer, TAG_PROFILE_INSTALLATION_RESULT).map_err(Error::Card)?;
    if !result.tag.constructed {
        return Err(Error::Invalid(
            "profile installation result is not constructed",
        ));
    }
    let transaction_id = required_unique_child(result, TAG_TRANSACTION_ID)
        .map_err(Error::Card)?
        .value;
    if transaction_id.is_empty() || transaction_id.len() > MAX_TRANSACTION_ID_BYTES {
        return Err(Error::Invalid(
            "profile installation transaction ID size is out of range",
        ));
    }
    let notification = unique_child(result, TAG_NOTIFICATION_METADATA)
        .map_err(Error::Card)?
        .map(|tlv| tlv.value);
    let final_result = required_unique_child(result, TAG_FINAL_RESULT).map_err(Error::Card)?;
    if !final_result.tag.constructed {
        return Err(Error::Invalid(
            "profile installation final result is not constructed",
        ));
    }
    let success = unique_child(final_result, TAG_SUCCESS_RESULT).map_err(Error::Card)?;
    let failure = unique_child(final_result, TAG_ERROR_RESULT).map_err(Error::Card)?;
    let isdp_aid = match (success, failure) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(Error::Invalid(
                "profile installation result branch is ambiguous",
            ));
        }
        (Some(success), None) => {
            if !success.tag.constructed {
                return Err(Error::Invalid(
                    "profile installation success result is not constructed",
                ));
            }
            let aid = required_unique_child(success, TAG_ISDP_AID)
                .map_err(Error::Card)?
                .value;
            if aid.is_empty() || aid.len() > MAX_PROFILE_AID_BYTES {
                return Err(Error::Invalid(
                    "installed profile ISD-P AID size is out of range",
                ));
            }
            aid
        }
        (None, Some(failure)) => {
            if !failure.tag.constructed {
                return Err(Error::Invalid(
                    "profile installation error result is not constructed",
                ));
            }
            let command_id = parse_small_integer(
                required_unique_child(failure, TAG_COMMAND_ID)
                    .map_err(Error::Card)?
                    .value,
            )
            .map_err(Error::Card)?;
            let reason = parse_small_integer(
                required_unique_child(failure, TAG_ERROR_REASON)
                    .map_err(Error::Card)?
                    .value,
            )
            .map_err(Error::Card)?;
            return Err(Error::ProfileInstall { command_id, reason });
        }
    };
    Ok(ProfileInstallResponse {
        raw: input,
        transaction_id,
        notification,
        isdp_aid,
    })
}

fn required_unique_child<'a>(parent: Tlv<'a>, tag: &[u8]) -> proletarius_ccid::Result<Tlv<'a>> {
    unique_child(parent, tag)?.ok_or(CardError::Invalid("required BER-TLV child is missing"))
}

fn unique_child<'a>(parent: Tlv<'a>, tag: &[u8]) -> proletarius_ccid::Result<Option<Tlv<'a>>> {
    let mut found = None;
    for item in parent.children()? {
        let item = item?;
        if item.matches(tag) {
            if found.is_some() {
                return Err(CardError::Invalid("duplicate BER-TLV child"));
            }
            found = Some(item);
        }
    }
    Ok(found)
}

#[derive(Clone, Copy)]
struct BoundProfileLayout<'a> {
    outer: LargeTlv<'a>,
    initial: LargeTlv<'a>,
    first_87: LargeTlv<'a>,
    sequence_88: LargeTlv<'a>,
    second_87: Option<LargeTlv<'a>>,
    sequence_86: LargeTlv<'a>,
}

impl<'a> BoundProfileLayout<'a> {
    fn parse(package: &'a [u8]) -> core::result::Result<Self, &'static str> {
        if package.is_empty() || package.len() > MAX_BOUND_PROFILE_PACKAGE_SIZE {
            return Err("Bound Profile Package size is out of range");
        }
        let outer = LargeTlv::parse(package)?;
        if outer.encoded.len() != package.len() {
            return Err("Bound Profile Package has trailing data");
        }
        outer.require(TAG_BOUND_PROFILE_PACKAGE, true)?;
        let mut children = outer.children()?;
        let initial = next_required(&mut children, "initialiseSecureChannelRequest is missing")?;
        initial.require(TAG_INITIALISE_SECURE_CHANNEL, true)?;
        let first_87 = next_required(&mut children, "firstSequenceOf87 is missing")?;
        first_87.require(TAG_FIRST_SEQUENCE_OF_87, true)?;
        let sequence_88 = next_required(&mut children, "sequenceOf88 is missing")?;
        sequence_88.require(TAG_SEQUENCE_OF_88, true)?;
        let next = next_required(&mut children, "sequenceOf86 is missing")?;
        let (second_87, sequence_86) = if next.tag == TAG_SECOND_SEQUENCE_OF_87 {
            next.require(TAG_SECOND_SEQUENCE_OF_87, true)?;
            (
                Some(next),
                next_required(&mut children, "sequenceOf86 is missing")?,
            )
        } else {
            (None, next)
        };
        sequence_86.require(TAG_SEQUENCE_OF_86, true)?;
        if children.next().is_some() {
            return Err("Bound Profile Package has unexpected top-level fields");
        }

        let mut segment_count = 1_usize;
        segment_count = checked_segment_count(
            segment_count,
            validate_sequence(first_87, TAG_PROFILE_ELEMENT_87, false)?,
        )?;
        segment_count = checked_segment_count(
            segment_count,
            validate_sequence(sequence_88, TAG_PROFILE_ELEMENT_88, true)?,
        )?;
        if let Some(second_87) = second_87 {
            segment_count = checked_segment_count(
                segment_count,
                validate_sequence(second_87, TAG_PROFILE_ELEMENT_87, false)?,
            )?;
        }
        checked_segment_count(
            segment_count,
            validate_sequence(sequence_86, TAG_PROFILE_ELEMENT_86, true)?,
        )?;
        Ok(Self {
            outer,
            initial,
            first_87,
            sequence_88,
            second_87,
            sequence_86,
        })
    }
}

fn next_required<'a>(
    children: &mut LargeChildren<'a>,
    missing: &'static str,
) -> core::result::Result<LargeTlv<'a>, &'static str> {
    children.next().ok_or(missing)?
}

fn validate_sequence(
    sequence: LargeTlv<'_>,
    expected_tag: &[u8],
    header_is_separate: bool,
) -> core::result::Result<usize, &'static str> {
    let mut count = usize::from(header_is_separate);
    let mut child_count = 0_usize;
    for child in sequence.children()? {
        let child = child?;
        child.require(expected_tag, false)?;
        child_count = child_count
            .checked_add(1)
            .ok_or("Bound Profile Package segment count overflow")?;
        count = checked_segment_count(count, 1)?;
    }
    if child_count == 0 {
        return Err("Bound Profile Package sequence is empty");
    }
    Ok(count)
}

fn checked_segment_count(
    current: usize,
    additional: usize,
) -> core::result::Result<usize, &'static str> {
    let total = current
        .checked_add(additional)
        .ok_or("Bound Profile Package segment count overflow")?;
    if total > MAX_BOUND_PROFILE_SEGMENTS {
        return Err("Bound Profile Package has too many segments");
    }
    Ok(total)
}

#[derive(Clone, Copy)]
struct LargeTlv<'a> {
    tag: &'a [u8],
    constructed: bool,
    encoded: &'a [u8],
    value: &'a [u8],
    header_length: usize,
}

impl<'a> LargeTlv<'a> {
    fn parse(input: &'a [u8]) -> core::result::Result<Self, &'static str> {
        let first = *input.first().ok_or("BER-TLV is truncated")?;
        let tag_length = if first & 0x1f == 0x1f {
            let mut end = 1_usize;
            loop {
                let byte = *input.get(end).ok_or("BER tag is truncated")?;
                end += 1;
                if end > 4 {
                    return Err("BER tag exceeds four bytes");
                }
                if byte & 0x80 == 0 {
                    break end;
                }
            }
        } else {
            1
        };
        let length_first = *input.get(tag_length).ok_or("BER length is truncated")?;
        let (value_length, length_length) = if length_first & 0x80 == 0 {
            (usize::from(length_first), 1)
        } else {
            let count = usize::from(length_first & 0x7f);
            if count == 0 {
                return Err("indefinite BER length is unsupported");
            }
            if count > 4 {
                return Err("BER length exceeds four bytes");
            }
            let bytes = input
                .get(tag_length + 1..tag_length + 1 + count)
                .ok_or("BER length is truncated")?;
            if bytes.first() == Some(&0) {
                return Err("BER length has a leading zero");
            }
            let mut length = 0_usize;
            for &byte in bytes {
                length = length
                    .checked_shl(8)
                    .and_then(|value| value.checked_add(usize::from(byte)))
                    .ok_or("BER length overflow")?;
            }
            (length, count + 1)
        };
        let header_length = tag_length
            .checked_add(length_length)
            .ok_or("BER header length overflow")?;
        let encoded_length = header_length
            .checked_add(value_length)
            .ok_or("BER value length overflow")?;
        if encoded_length > MAX_BOUND_PROFILE_PACKAGE_SIZE {
            return Err("BER-TLV exceeds the Bound Profile Package size limit");
        }
        let encoded = input
            .get(..encoded_length)
            .ok_or("BER-TLV value is truncated")?;
        Ok(Self {
            tag: &input[..tag_length],
            constructed: first & 0x20 != 0,
            encoded,
            value: &encoded[header_length..],
            header_length,
        })
    }

    fn require(self, tag: &[u8], constructed: bool) -> core::result::Result<(), &'static str> {
        if self.tag != tag || self.constructed != constructed {
            return Err("Bound Profile Package contains an unexpected BER-TLV");
        }
        Ok(())
    }

    fn children(self) -> core::result::Result<LargeChildren<'a>, &'static str> {
        if !self.constructed {
            return Err("primitive Bound Profile Package TLV has no children");
        }
        Ok(LargeChildren { input: self.value })
    }
}

struct LargeChildren<'a> {
    input: &'a [u8],
}

impl<'a> Iterator for LargeChildren<'a> {
    type Item = core::result::Result<LargeTlv<'a>, &'static str>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.input.is_empty() {
            return None;
        }
        match LargeTlv::parse(self.input) {
            Ok(tlv) => {
                self.input = &self.input[tlv.encoded.len()..];
                Some(Ok(tlv))
            }
            Err(error) => {
                self.input = &[];
                Some(Err(error))
            }
        }
    }
}
