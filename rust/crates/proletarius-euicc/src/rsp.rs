// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! Card-side ES10b commands used by Remote SIM Provisioning.
//!
//! Values received from an SM-DP+ remain encoded BER-TLV objects.  This keeps
//! certificate and signature bytes opaque to the LPA and lets the eUICC remain
//! the trust anchor.

use alloc::{string::String, vec::Vec};
use core::mem;

use proletarius_ccid::{Error as CardError, bertlv};

use super::{
    CardTransport, Error, Euicc, EuiccInfoVersion, MAX_NOTIFICATION_COUNT, NotificationOperation,
    NotificationResult, Result, child, encode_unsigned_integer, parse_small_integer,
    parse_unsigned_integer, single_tlv,
};

const TAG_GET_EUICC_CHALLENGE: &[u8] = &[0xbf, 0x2e];
const TAG_AUTHENTICATE_SERVER: &[u8] = &[0xbf, 0x38];
const TAG_PREPARE_DOWNLOAD: &[u8] = &[0xbf, 0x21];
const TAG_CANCEL_SESSION: &[u8] = &[0xbf, 0x41];
const TAG_RETRIEVE_NOTIFICATIONS: &[u8] = &[0xbf, 0x2b];
const TAG_NOTIFICATION_CONTAINER: &[u8] = &[0xa0];
const TAG_NOTIFICATIONS_ERROR: &[u8] = &[0x81];
const TAG_NOTIFICATION_METADATA: &[u8] = &[0xbf, 0x2f];
const TAG_SEQUENCE_NUMBER: &[u8] = &[0x80];
const TAG_NOTIFICATION_ADDRESS: &[u8] = &[0x0c];
const TAG_SUCCESS: &[u8] = &[0xa0];
const TAG_ERROR: &[u8] = &[0xa1];
const TAG_DEVICE_INFO: &[u8] = &[0xa1];
const TAG_DEVICE_CAPABILITIES: &[u8] = &[0xa1];
const TAG_TAC: &[u8] = &[0x80];
const TAG_IMEI: &[u8] = &[0x82];
const TAG_MATCHING_ID: &[u8] = &[0x80];
const TAG_CONTEXT_PARAMETERS: &[u8] = &[0xa0];
const TAG_CONFIRMATION_CODE_HASH: &[u8] = &[0x04];
const MAX_TRANSACTION_ID_BYTES: usize = 64;
const MAX_MATCHING_ID_BYTES: usize = 255;
const MAX_IMEI_DIGITS: usize = 16;
const MAX_NOTIFICATION_BYTES: usize = 65_535;
const MAX_NOTIFICATION_ADDRESS_BYTES: usize = 512;

/// Opaque SM-DP+ objects needed by `ES10b.AuthenticateServer`.
#[derive(Clone, Copy, Debug)]
pub struct ServerAuthenticationRequest<'a> {
    pub signed1: &'a [u8],
    pub signature1: &'a [u8],
    pub euicc_ci_pk_id: &'a [u8],
    pub server_certificate: &'a [u8],
    pub matching_id: &'a [u8],
    pub imei: &'a str,
}

/// Opaque SM-DP+ objects needed by `ES10b.PrepareDownload`.
#[derive(Clone, Copy, Debug)]
pub struct PrepareDownloadRequest<'a> {
    pub signed2: &'a [u8],
    pub signature2: &'a [u8],
    pub server_certificate: &'a [u8],
    /// Double-SHA256 confirmation-code value defined by SGP.22, if required.
    pub confirmation_code_hash: Option<&'a [u8]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CancelSessionReason {
    EndUserRejection = 0,
    Postponed = 1,
    Timeout = 2,
    PprNotAllowed = 3,
    MetadataMismatch = 4,
    LoadBppExecutionError = 5,
    Undefined = 127,
}

/// Owned result of a successfully installed profile. Notifications remain in
/// the eUICC notification queue and are retrieved independently by the
/// application's asynchronous delivery worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledProfile {
    pub transaction_id: Vec<u8>,
    pub isdp_aid: Vec<u8>,
}

/// One complete pending notification ready for `ES9+.HandleNotification`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingNotification {
    pub sequence_number: u64,
    pub address: String,
    pub encoded: Vec<u8>,
}

/// Read the confirmation-code-required BOOLEAN embedded in `smdpSigned2`.
/// Missing values mean that no confirmation code is required.
pub fn confirmation_code_required(input: &[u8]) -> proletarius_ccid::Result<bool> {
    let outer = single_tlv(input)?;
    let Some(value) = find_nested_card(outer, &[0x01], 3)? else {
        return Ok(false);
    };
    match value.value {
        [0] => Ok(false),
        [value] => Ok(*value != 0),
        _ => Err(CardError::Invalid(
            "confirmation-code indicator is malformed",
        )),
    }
}

impl<T: CardTransport> Euicc<T> {
    /// Return the challenge value from `ES10b.GetEUICCChallenge`.
    pub fn get_euicc_challenge(&mut self) -> Result<Vec<u8>, T::Error> {
        self.request.clear();
        bertlv::encode_constructed(TAG_GET_EUICC_CHALLENGE, &[], &mut self.request)
            .map_err(Error::Card)?;
        self.exchange_request()?;
        let outer = expect_constructed(&self.payload, TAG_GET_EUICC_CHALLENGE)?;
        let challenge = outer
            .children()
            .map_err(Error::Card)?
            .next()
            .ok_or(Error::Invalid("GetEUICCChallenge response is empty"))?
            .map_err(Error::Card)?;
        if challenge.tag.bytes != TAG_SEQUENCE_NUMBER
            || challenge.tag.constructed
            || challenge.value.is_empty()
            || challenge.value.len() > 64
        {
            return Err(Error::Invalid("eUICC challenge size is out of range"));
        }
        Ok(challenge.value.to_vec())
    }

    /// Return the complete encoded EUICCInfo object for ES9+ JSON transport.
    pub fn get_euicc_info_raw(&mut self, version: EuiccInfoVersion) -> Result<Vec<u8>, T::Error> {
        self.request.clear();
        bertlv::encode_constructed(version.tag(), &[], &mut self.request).map_err(Error::Card)?;
        self.exchange_request()?;
        expect_constructed(&self.payload, version.tag())?;
        Ok(self.payload.clone())
    }

    /// Authenticate the SM-DP+ to the eUICC and return the complete encoded
    /// `authenticateServerResponse` value for ES9+.
    pub fn authenticate_server(
        &mut self,
        input: ServerAuthenticationRequest<'_>,
    ) -> Result<Vec<u8>, T::Error> {
        validate_opaque_tlv(input.signed1)?;
        validate_opaque_tlv(input.signature1)?;
        validate_opaque_tlv(input.euicc_ci_pk_id)?;
        validate_opaque_tlv(input.server_certificate)?;
        if input.matching_id.len() > MAX_MATCHING_ID_BYTES {
            return Err(Error::Invalid("matching ID is too large"));
        }
        let (imei, include_imei) = if input.imei.is_empty() {
            (Vec::from([0x35, 0x29, 0x06, 0x11]), false)
        } else {
            (encode_imei(input.imei)?, true)
        };

        self.request.clear();
        self.request.extend_from_slice(input.signed1);
        self.request.extend_from_slice(input.signature1);
        self.request.extend_from_slice(input.euicc_ci_pk_id);
        self.request.extend_from_slice(input.server_certificate);

        self.apdu.clear();
        bertlv::encode(TAG_TAC, &imei[..4], &mut self.apdu).map_err(Error::Card)?;
        bertlv::encode_constructed(TAG_DEVICE_CAPABILITIES, &[], &mut self.apdu)
            .map_err(Error::Card)?;
        if include_imei {
            bertlv::encode(TAG_IMEI, &imei, &mut self.apdu).map_err(Error::Card)?;
        }
        let mut device = Vec::with_capacity(self.apdu.len() + 4);
        bertlv::encode_constructed(TAG_DEVICE_INFO, &self.apdu, &mut device)
            .map_err(Error::Card)?;
        self.apdu.clear();
        bertlv::encode(TAG_MATCHING_ID, input.matching_id, &mut self.apdu).map_err(Error::Card)?;
        self.apdu.extend_from_slice(&device);
        let mut context = Vec::with_capacity(self.apdu.len() + 4);
        bertlv::encode_constructed(TAG_CONTEXT_PARAMETERS, &self.apdu, &mut context)
            .map_err(Error::Card)?;
        self.request.extend_from_slice(&context);

        self.apdu.clear();
        bertlv::encode_constructed(TAG_AUTHENTICATE_SERVER, &self.request, &mut self.apdu)
            .map_err(Error::Card)?;
        mem::swap(&mut self.request, &mut self.apdu);
        self.exchange_request()?;
        let outer = expect_constructed(&self.payload, TAG_AUTHENTICATE_SERVER)?;
        if child(outer, TAG_SUCCESS).map_err(Error::Card)?.is_none() {
            if child(outer, TAG_ERROR).map_err(Error::Card)?.is_some() {
                return Err(Error::Invalid("eUICC rejected AuthenticateServer"));
            }
            return Err(Error::Invalid("AuthenticateServer response has no result"));
        }
        Ok(self.payload.clone())
    }

    /// Prepare the eUICC and return the complete encoded
    /// `prepareDownloadResponse` value for ES9+.
    pub fn prepare_download(
        &mut self,
        input: PrepareDownloadRequest<'_>,
    ) -> Result<Vec<u8>, T::Error> {
        validate_opaque_tlv(input.signed2)?;
        validate_opaque_tlv(input.signature2)?;
        validate_opaque_tlv(input.server_certificate)?;
        if input
            .confirmation_code_hash
            .is_some_and(|value| value.len() != 32)
        {
            return Err(Error::Invalid("confirmation code hash must be 32 bytes"));
        }
        self.request.clear();
        self.request.extend_from_slice(input.signed2);
        self.request.extend_from_slice(input.signature2);
        if let Some(hash) = input.confirmation_code_hash {
            bertlv::encode(TAG_CONFIRMATION_CODE_HASH, hash, &mut self.request)
                .map_err(Error::Card)?;
        }
        self.request.extend_from_slice(input.server_certificate);
        self.apdu.clear();
        bertlv::encode_constructed(TAG_PREPARE_DOWNLOAD, &self.request, &mut self.apdu)
            .map_err(Error::Card)?;
        mem::swap(&mut self.request, &mut self.apdu);
        self.exchange_request()?;
        expect_constructed(&self.payload, TAG_PREPARE_DOWNLOAD)?;
        Ok(self.payload.clone())
    }

    /// Cancel the eUICC session and return the encoded response for the
    /// matching ES9+ cancellation call.
    pub fn cancel_rsp_session(
        &mut self,
        transaction_id: &[u8],
        reason: CancelSessionReason,
    ) -> Result<Vec<u8>, T::Error> {
        validate_transaction_id(transaction_id)?;
        self.apdu.clear();
        bertlv::encode(TAG_SEQUENCE_NUMBER, transaction_id, &mut self.apdu).map_err(Error::Card)?;
        bertlv::encode(&[0x81], &[reason as u8], &mut self.apdu).map_err(Error::Card)?;
        self.request.clear();
        bertlv::encode_constructed(TAG_CANCEL_SESSION, &self.apdu, &mut self.request)
            .map_err(Error::Card)?;
        self.exchange_request()?;
        expect_constructed(&self.payload, TAG_CANCEL_SESSION)?;
        Ok(self.payload.clone())
    }

    /// Load a BPP while retaining the complete notification needed by ES9+.
    pub fn install_bound_profile_package(
        &mut self,
        package: &[u8],
    ) -> Result<InstalledProfile, T::Error> {
        let response = self.load_bound_profile_package(package)?;
        let transaction_id = response.transaction_id().to_vec();
        let isdp_aid = response.isdp_aid().to_vec();
        Ok(InstalledProfile {
            transaction_id,
            isdp_aid,
        })
    }

    /// Retrieve the first complete notification by sequence number.
    ///
    /// Call [`Self::retrieve_notifications`] when acknowledging a sequence:
    /// every returned notification must be delivered before removing it.
    pub fn retrieve_notification(
        &mut self,
        sequence_number: u64,
    ) -> Result<Option<PendingNotification>, T::Error> {
        Ok(self
            .retrieve_notifications(sequence_number)?
            .into_iter()
            .next())
    }

    /// Retrieve every complete notification matching a sequence number.
    ///
    /// A sequence may have multiple pending payloads or delivery addresses.
    /// The whole response is validated before returning any of its records,
    /// so a malformed later record cannot lead to premature acknowledgement.
    pub fn retrieve_notifications(
        &mut self,
        sequence_number: u64,
    ) -> Result<Vec<PendingNotification>, T::Error> {
        let mut integer = [0_u8; 9];
        let integer_len = encode_unsigned_integer(sequence_number, &mut integer);
        self.apdu.clear();
        bertlv::encode(TAG_SEQUENCE_NUMBER, &integer[..integer_len], &mut self.apdu)
            .map_err(Error::Card)?;
        self.request.clear();
        bertlv::encode_constructed(TAG_NOTIFICATION_CONTAINER, &self.apdu, &mut self.request)
            .map_err(Error::Card)?;
        self.apdu.clear();
        bertlv::encode_constructed(TAG_RETRIEVE_NOTIFICATIONS, &self.request, &mut self.apdu)
            .map_err(Error::Card)?;
        mem::swap(&mut self.request, &mut self.apdu);
        self.exchange_request()?;
        let outer = expect_constructed(&self.payload, TAG_RETRIEVE_NOTIFICATIONS)?;
        if let Some(error) = child(outer, TAG_NOTIFICATIONS_ERROR).map_err(Error::Card)? {
            return Err(Error::NotificationOperation {
                operation: NotificationOperation::Retrieve,
                result: NotificationResult::from_code(
                    parse_small_integer(error.value).map_err(Error::Card)?,
                ),
            });
        }
        let container = child(outer, TAG_NOTIFICATION_CONTAINER)
            .map_err(Error::Card)?
            .ok_or(Error::Invalid("notification list is missing"))?;
        let mut notifications = Vec::new();
        for (index, candidate) in container.children().map_err(Error::Card)?.enumerate() {
            if index >= MAX_NOTIFICATION_COUNT {
                return Err(Error::Invalid("notification list exceeds the record limit"));
            }
            let candidate = candidate.map_err(Error::Card)?;
            let metadata = find_nested(candidate, TAG_NOTIFICATION_METADATA, 3)?
                .ok_or(Error::Invalid("notification metadata is missing"))?;
            let (sequence, address) = parse_notification_tlv(metadata)?;
            if sequence != sequence_number {
                continue;
            }
            let mut encoded = Vec::with_capacity(candidate.value.len() + 8);
            bertlv::encode(candidate.tag.bytes, candidate.value, &mut encoded)
                .map_err(Error::Card)?;
            if encoded.len() > MAX_NOTIFICATION_BYTES {
                return Err(Error::ResponseTooLarge);
            }
            notifications.push(PendingNotification {
                sequence_number: sequence,
                address,
                encoded,
            });
        }
        Ok(notifications)
    }
}

fn expect_constructed<'a, E>(
    input: &'a [u8],
    expected: &[u8],
) -> Result<proletarius_ccid::bertlv::Tlv<'a>, E> {
    let outer = single_tlv(input).map_err(Error::Card)?;
    if outer.tag.bytes != expected || !outer.tag.constructed {
        return Err(Error::Invalid("unexpected ES10b response tag"));
    }
    Ok(outer)
}

fn validate_opaque_tlv<E>(input: &[u8]) -> Result<(), E> {
    if input.is_empty() || input.len() > 65_535 {
        return Err(Error::Invalid("opaque BER-TLV size is out of range"));
    }
    single_tlv(input).map_err(Error::Card)?;
    Ok(())
}

fn validate_transaction_id<E>(transaction_id: &[u8]) -> Result<(), E> {
    if transaction_id.is_empty() || transaction_id.len() > MAX_TRANSACTION_ID_BYTES {
        return Err(Error::Invalid("transaction ID size is out of range"));
    }
    Ok(())
}

fn encode_imei<E>(imei: &str) -> Result<Vec<u8>, E> {
    if !(8..=MAX_IMEI_DIGITS).contains(&imei.len())
        || !imei.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::Invalid("IMEI must contain 8 to 16 decimal digits"));
    }
    let mut encoded = Vec::with_capacity(8);
    for pair in imei.as_bytes().chunks(2) {
        let low = pair[0] - b'0';
        let high = pair.get(1).map_or(0x0f, |value| value - b'0');
        encoded.push(low | (high << 4));
    }
    if encoded.len() != 4 && encoded.len() != 8 {
        return Err(Error::Invalid("IMEI must encode to 4 or 8 bytes"));
    }
    Ok(encoded)
}

fn parse_notification_tlv<E>(
    metadata: proletarius_ccid::bertlv::Tlv<'_>,
) -> Result<(u64, String), E> {
    let sequence = child(metadata, TAG_SEQUENCE_NUMBER)
        .map_err(Error::Card)?
        .map(|value| value.value);
    let address = child(metadata, TAG_NOTIFICATION_ADDRESS)
        .map_err(Error::Card)?
        .map(|value| value.value);
    parse_notification_fields(sequence, address)
}

fn parse_notification_fields<E>(
    sequence: Option<&[u8]>,
    address: Option<&[u8]>,
) -> Result<(u64, String), E> {
    let sequence = sequence.ok_or(Error::Invalid("notification sequence number is missing"))?;
    let address = address.ok_or(Error::Invalid("notification address is missing"))?;
    if address.is_empty() || address.len() > MAX_NOTIFICATION_ADDRESS_BYTES {
        return Err(Error::Invalid("notification address size is out of range"));
    }
    let address = core::str::from_utf8(address)
        .map_err(|_| Error::Invalid("notification address is not UTF-8"))?;
    Ok((
        parse_unsigned_integer(sequence).map_err(Error::Card)?,
        String::from(address),
    ))
}

fn find_nested<'a, E>(
    root: proletarius_ccid::bertlv::Tlv<'a>,
    tag: &[u8],
    remaining_depth: u8,
) -> Result<Option<proletarius_ccid::bertlv::Tlv<'a>>, E> {
    if root.tag.bytes == tag {
        return Ok(Some(root));
    }
    if !root.tag.constructed || remaining_depth == 0 {
        return Ok(None);
    }
    for child in root.children().map_err(Error::Card)? {
        let child = child.map_err(Error::Card)?;
        if let Some(found) = find_nested(child, tag, remaining_depth - 1)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn find_nested_card<'a>(
    root: proletarius_ccid::bertlv::Tlv<'a>,
    tag: &[u8],
    remaining_depth: u8,
) -> proletarius_ccid::Result<Option<proletarius_ccid::bertlv::Tlv<'a>>> {
    if root.tag.bytes == tag {
        return Ok(Some(root));
    }
    if !root.tag.constructed || remaining_depth == 0 {
        return Ok(None);
    }
    for child in root.children()? {
        let child = child?;
        if let Some(found) = find_nested_card(child, tag, remaining_depth - 1)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    struct FakeCard {
        response: Option<Vec<u8>>,
        commands: Vec<Vec<u8>>,
    }

    impl CardTransport for FakeCard {
        type Error = &'static str;

        fn transmit(
            &mut self,
            command: &[u8],
            response: &mut Vec<u8>,
        ) -> core::result::Result<(), Self::Error> {
            self.commands.push(command.to_vec());
            *response = self.response.take().ok_or("no scripted response")?;
            Ok(())
        }
    }

    fn card_response(body: &[u8]) -> Euicc<FakeCard> {
        let mut response = Vec::new();
        bertlv::encode_constructed(TAG_RETRIEVE_NOTIFICATIONS, body, &mut response).unwrap();
        response.extend_from_slice(&[0x90, 0x00]);
        Euicc::with_default_mss(
            FakeCard {
                response: Some(response),
                commands: Vec::new(),
            },
            0,
        )
        .unwrap()
    }

    fn notification(sequence: u64, address: &[u8], install: bool) -> Vec<u8> {
        let mut integer = [0; 9];
        let len = encode_unsigned_integer(sequence, &mut integer);
        let mut fields = Vec::new();
        bertlv::encode(TAG_SEQUENCE_NUMBER, &integer[..len], &mut fields).unwrap();
        bertlv::encode(TAG_NOTIFICATION_ADDRESS, address, &mut fields).unwrap();
        let mut metadata = Vec::new();
        bertlv::encode_constructed(TAG_NOTIFICATION_METADATA, &fields, &mut metadata).unwrap();
        let mut encoded = Vec::new();
        if install {
            let mut signed = Vec::new();
            bertlv::encode_constructed(&[0xbf, 0x27], &metadata, &mut signed).unwrap();
            bertlv::encode_constructed(&[0xbf, 0x37], &signed, &mut encoded).unwrap();
        } else {
            bertlv::encode_constructed(&[0x30], &metadata, &mut encoded).unwrap();
        }
        encoded
    }

    fn list_response(entries: &[Vec<u8>]) -> Euicc<FakeCard> {
        let records: Vec<u8> = entries
            .iter()
            .flat_map(|entry| entry.iter().copied())
            .collect();
        let mut container = Vec::new();
        bertlv::encode_constructed(TAG_NOTIFICATION_CONTAINER, &records, &mut container).unwrap();
        card_response(&container)
    }

    #[test]
    fn retrieve_notifications_preserves_all_matching_payloads_and_order() {
        let first = notification(7, b"first.example", false);
        let unrelated = notification(8, b"other.example", false);
        let second = notification(7, b"second.example", true);
        let mut card = list_response(&[first.clone(), unrelated, second.clone()]);

        assert_eq!(
            card.retrieve_notifications(7).unwrap(),
            vec![
                PendingNotification {
                    sequence_number: 7,
                    address: String::from("first.example"),
                    encoded: first
                },
                PendingNotification {
                    sequence_number: 7,
                    address: String::from("second.example"),
                    encoded: second
                },
            ]
        );
        assert_eq!(
            card.into_inner().commands,
            vec![vec![
                0x80, 0xe2, 0x91, 0x00, 0x08, 0xbf, 0x2b, 0x05, 0xa0, 0x03, 0x80, 0x01, 0x07,
            ]]
        );
    }

    #[test]
    fn retrieve_notification_compatibility_returns_first_match() {
        let first = notification(7, b"first.example", false);
        let second = notification(7, b"second.example", true);
        let mut card = list_response(&[first.clone(), second]);
        assert_eq!(
            card.retrieve_notification(7).unwrap().unwrap().encoded,
            first
        );
        assert_eq!(card.into_inner().commands.len(), 1);
    }

    #[test]
    fn retrieve_notifications_empty_and_unmatched_lists_return_no_records() {
        assert_eq!(
            list_response(&[]).retrieve_notifications(7).unwrap(),
            vec![]
        );
        assert_eq!(list_response(&[]).retrieve_notification(7).unwrap(), None);
        assert_eq!(
            list_response(&[notification(8, b"other.example", false)])
                .retrieve_notifications(7)
                .unwrap(),
            vec![]
        );
    }

    #[test]
    fn retrieve_notifications_card_error_is_not_an_empty_list() {
        let mut card = card_response(&[0x81, 0x01, 0x7f]);
        assert_eq!(
            card.retrieve_notifications(7),
            Err(Error::NotificationOperation {
                operation: NotificationOperation::Retrieve,
                result: NotificationResult::Undefined,
            })
        );
        assert!(
            card_response(&[0x81, 0x00])
                .retrieve_notifications(7)
                .is_err()
        );
        assert!(card_response(&[]).retrieve_notifications(7).is_err());
    }

    #[test]
    fn retrieve_notifications_rejects_malformed_later_record_without_partial_success() {
        let records = [notification(7, b"first.example", false), vec![0x30, 0x00]];
        assert!(list_response(&records).retrieve_notifications(7).is_err());
        // The compatibility API must validate the remainder before returning
        // its first record, too; callers may acknowledge the sequence next.
        assert!(list_response(&records).retrieve_notification(7).is_err());
        assert!(
            list_response(&[notification(7, &[0xff], false)])
                .retrieve_notifications(7)
                .is_err()
        );
    }

    #[test]
    fn retrieve_notifications_rejects_truncated_list_after_valid_record() {
        let mut records = notification(7, b"first.example", false);
        records.extend_from_slice(&[0x30, 0x02, 0x00]);
        let mut container = Vec::new();
        bertlv::encode_constructed(TAG_NOTIFICATION_CONTAINER, &records, &mut container).unwrap();
        assert!(card_response(&container).retrieve_notifications(7).is_err());
    }

    #[test]
    fn retrieve_notifications_bounds_all_records_including_unmatched_ones() {
        let record = notification(7, b"example.com", false);
        let entries = vec![record; MAX_NOTIFICATION_COUNT];
        assert_eq!(
            list_response(&entries)
                .retrieve_notifications(7)
                .unwrap()
                .len(),
            MAX_NOTIFICATION_COUNT
        );
        let mut oversized = entries;
        oversized.push(notification(8, b"other.example", false));
        assert_eq!(
            list_response(&oversized).retrieve_notifications(7),
            Err(Error::Invalid("notification list exceeds the record limit"))
        );
        assert_eq!(
            list_response(&oversized).retrieve_notifications(9),
            Err(Error::Invalid("notification list exceeds the record limit"))
        );
    }

    #[test]
    fn retrieve_notifications_retains_full_unsigned_sequence_number() {
        let mut card = list_response(&[notification(u64::MAX, b"example.com", false)]);
        assert_eq!(
            card.retrieve_notifications(u64::MAX).unwrap()[0].sequence_number,
            u64::MAX
        );
    }

    #[test]
    fn retrieve_notifications_propagates_transport_and_status_errors() {
        let mut card = Euicc::with_default_mss(
            FakeCard {
                response: None,
                commands: Vec::new(),
            },
            0,
        )
        .unwrap();
        assert_eq!(
            card.retrieve_notifications(7),
            Err(Error::Transport("no scripted response"))
        );
        let mut card = Euicc::with_default_mss(
            FakeCard {
                response: Some(vec![0x69, 0x85]),
                commands: Vec::new(),
            },
            0,
        )
        .unwrap();
        assert_eq!(
            card.retrieve_notifications(7),
            Err(Error::Card(CardError::CardStatus(0x6985)))
        );
    }
}
