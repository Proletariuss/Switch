#![forbid(unsafe_code)]

mod address;
pub mod alphabet;
pub mod binary_sms;
pub mod deliver;
mod oma_cp;
pub mod reassembly;
pub mod segments;
pub mod septets;
pub mod status_report;
pub mod submit;
pub mod timestamp;
pub mod udh;
pub mod user_data;
pub mod ussd;

use std::collections::HashMap;
use std::fmt;

pub const SMS_PROTOCOL_CONTRACT: &str = "3gpp-rpdu-delivery-v1";
const MAX_TPDU_LENGTH: usize = 232;
const MAX_RPDU_LENGTH: usize = 255;
const MAX_PENDING_DELIVERIES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SmsError {
    Empty,
    MessageTooLarge,
    WrongMessageType(u8),
    Truncated(&'static str),
    TrailingData,
    InvalidAddress,
    InvalidUserDataLength,
    InvalidCause,
    DuplicatePendingReference,
    DeliveryCapacityExceeded,
    UnknownDeliveryReference,
}

impl fmt::Display for SmsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("RPDU is empty"),
            Self::MessageTooLarge => formatter.write_str("RPDU exceeds 255 octets"),
            Self::WrongMessageType(value) => {
                write!(formatter, "unsupported RP message type {value:#04x}")
            }
            Self::Truncated(field) => write!(formatter, "RPDU field is truncated: {field}"),
            Self::TrailingData => formatter.write_str("RPDU contains trailing data"),
            Self::InvalidAddress => formatter.write_str("invalid RP address"),
            Self::InvalidUserDataLength => formatter.write_str("invalid RP user-data length"),
            Self::InvalidCause => formatter.write_str("invalid RP cause information element"),
            Self::DuplicatePendingReference => {
                formatter.write_str("RP message reference is already pending")
            }
            Self::DeliveryCapacityExceeded => {
                formatter.write_str("pending SMS delivery capacity exceeded")
            }
            Self::UnknownDeliveryReference => {
                formatter.write_str("delivery report has no pending RP reference")
            }
        }
    }
}

impl std::error::Error for SmsError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MobileTerminatedData {
    pub reference: u8,
    pub originator: Option<String>,
    pub destination: Option<String>,
    pub tpdu: Vec<u8>,
}

impl MobileTerminatedData {
    pub fn parse(data: &[u8]) -> Result<Self, SmsError> {
        if data.is_empty() {
            return Err(SmsError::Empty);
        }
        if data.len() > MAX_RPDU_LENGTH {
            return Err(SmsError::MessageTooLarge);
        }
        if data[0] != 0x01 {
            return Err(SmsError::WrongMessageType(data[0]));
        }
        let reference = *data
            .get(1)
            .ok_or(SmsError::Truncated("message reference"))?;
        let mut offset = 2;
        let originator = parse_address_field(data, &mut offset, "originator address")?;
        let destination = parse_address_field(data, &mut offset, "destination address")?;
        let length = usize::from(
            *data
                .get(offset)
                .ok_or(SmsError::Truncated("user-data length"))?,
        );
        offset += 1;
        if length == 0 || length > MAX_TPDU_LENGTH {
            return Err(SmsError::InvalidUserDataLength);
        }
        let end = offset
            .checked_add(length)
            .ok_or(SmsError::InvalidUserDataLength)?;
        let tpdu = data
            .get(offset..end)
            .ok_or(SmsError::Truncated("user data"))?;
        if end != data.len() {
            return Err(SmsError::TrailingData);
        }
        Ok(Self {
            reference,
            originator,
            destination,
            tpdu: tpdu.to_vec(),
        })
    }
}

fn parse_address_field(
    data: &[u8],
    offset: &mut usize,
    field: &'static str,
) -> Result<Option<String>, SmsError> {
    let length = usize::from(*data.get(*offset).ok_or(SmsError::Truncated(field))?);
    *offset += 1;
    let end = offset.checked_add(length).ok_or(SmsError::InvalidAddress)?;
    let encoded = data.get(*offset..end).ok_or(SmsError::Truncated(field))?;
    *offset = end;
    if encoded.is_empty() {
        return Ok(None);
    }
    decode_address(encoded).map(Some)
}

fn decode_address(encoded: &[u8]) -> Result<String, SmsError> {
    let (&toa, digits) = encoded.split_first().ok_or(SmsError::InvalidAddress)?;
    if digits.is_empty() || toa & 0x80 == 0 {
        return Err(SmsError::InvalidAddress);
    }
    let mut result = String::with_capacity(digits.len() * 2 + 1);
    if toa & 0x70 == 0x10 {
        result.push('+');
    }
    for (index, octet) in digits.iter().copied().enumerate() {
        let low = octet & 0x0f;
        let high = octet >> 4;
        if low > 9 {
            return Err(SmsError::InvalidAddress);
        }
        result.push(char::from(b'0' + low));
        if high == 0x0f {
            if index + 1 != digits.len() {
                return Err(SmsError::InvalidAddress);
            }
        } else if high <= 9 {
            result.push(char::from(b'0' + high));
        } else {
            return Err(SmsError::InvalidAddress);
        }
    }
    Ok(result)
}

pub fn build_mobile_originated_data(
    reference: u8,
    smsc: &str,
    tpdu: &[u8],
) -> Result<Vec<u8>, SmsError> {
    if tpdu.is_empty() || tpdu.len() > MAX_TPDU_LENGTH {
        return Err(SmsError::InvalidUserDataLength);
    }
    let address = encode_address(smsc)?;
    let length = 5usize
        .checked_add(address.len())
        .and_then(|value| value.checked_add(tpdu.len()))
        .ok_or(SmsError::MessageTooLarge)?;
    if length > MAX_RPDU_LENGTH {
        return Err(SmsError::MessageTooLarge);
    }
    let mut result = Vec::with_capacity(length);
    result.extend_from_slice(&[0x00, reference, 0x00, address.len() as u8]);
    result.extend_from_slice(&address);
    result.push(tpdu.len() as u8);
    result.extend_from_slice(tpdu);
    Ok(result)
}

fn encode_address(value: &str) -> Result<Vec<u8>, SmsError> {
    // Preserve the existing card/SMSC normalization at the migration boundary.
    let normalized: String = value
        .trim()
        .chars()
        .enumerate()
        .filter_map(|(index, ch)| (ch.is_ascii_digit() || (index == 0 && ch == '+')).then_some(ch))
        .collect();
    let trimmed = normalized.as_str();
    let (international, digits) = match trimmed.strip_prefix('+') {
        Some(digits) => (true, digits),
        None => (false, trimmed),
    };
    if !(3..=20).contains(&digits.len()) || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SmsError::InvalidAddress);
    }
    let mut result = Vec::with_capacity(1 + digits.len().div_ceil(2));
    result.push(if international { 0x91 } else { 0x81 });
    for pair in digits.as_bytes().chunks(2) {
        let low = pair[0] - b'0';
        let high = pair.get(1).map_or(0x0f, |value| *value - b'0');
        result.push((high << 4) | low);
    }
    Ok(result)
}

pub fn build_ack(reference: u8) -> [u8; 2] {
    [0x02, reference]
}

pub fn build_error(reference: u8, cause: u8) -> [u8; 4] {
    [0x04, reference, 0x01, cause & 0x7f]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryReport {
    Ack { reference: u8 },
    Error { reference: u8, cause: u8 },
}

impl DeliveryReport {
    pub fn parse(data: &[u8]) -> Result<Self, SmsError> {
        if data.len() < 2 {
            return Err(SmsError::Truncated("delivery report"));
        }
        if data.len() > MAX_RPDU_LENGTH {
            return Err(SmsError::MessageTooLarge);
        }
        match data[0] {
            0x02 | 0x03 => {
                parse_optional_user_data(data, 2)?;
                Ok(Self::Ack { reference: data[1] })
            }
            0x04 | 0x05 => {
                let cause_length =
                    usize::from(*data.get(2).ok_or(SmsError::Truncated("cause length"))?);
                if cause_length == 0 {
                    return Err(SmsError::InvalidCause);
                }
                let end = 3usize
                    .checked_add(cause_length)
                    .ok_or(SmsError::InvalidCause)?;
                let cause = *data.get(3).ok_or(SmsError::Truncated("cause"))? & 0x7f;
                if end > data.len() {
                    return Err(SmsError::Truncated("cause"));
                }
                parse_optional_user_data(data, end)?;
                Ok(Self::Error {
                    reference: data[1],
                    cause,
                })
            }
            other => Err(SmsError::WrongMessageType(other)),
        }
    }

    pub fn reference(self) -> u8 {
        match self {
            Self::Ack { reference } | Self::Error { reference, .. } => reference,
        }
    }
}

fn parse_optional_user_data(data: &[u8], offset: usize) -> Result<(), SmsError> {
    if offset == data.len() {
        return Ok(());
    }
    let length = usize::from(
        *data
            .get(offset)
            .ok_or(SmsError::Truncated("optional user-data length"))?,
    );
    let end = offset
        .checked_add(1)
        .and_then(|value| value.checked_add(length))
        .ok_or(SmsError::InvalidUserDataLength)?;
    if end > data.len() {
        return Err(SmsError::Truncated("optional user data"));
    }
    if end != data.len() {
        return Err(SmsError::TrailingData);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingDelivery {
    pub message_id: String,
    pub part: u16,
}

#[derive(Debug, Default)]
pub struct DeliveryTracker {
    pending: HashMap<u8, PendingDelivery>,
}

impl DeliveryTracker {
    pub fn insert(&mut self, reference: u8, delivery: PendingDelivery) -> Result<(), SmsError> {
        if self.pending.contains_key(&reference) {
            return Err(SmsError::DuplicatePendingReference);
        }
        if self.pending.len() >= MAX_PENDING_DELIVERIES {
            return Err(SmsError::DeliveryCapacityExceeded);
        }
        self.pending.insert(reference, delivery);
        Ok(())
    }

    pub fn complete(&mut self, report: DeliveryReport) -> Result<PendingDelivery, SmsError> {
        let reference = report.reference();
        self.pending
            .remove(&reference)
            .ok_or(SmsError::UnknownDeliveryReference)
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

/// SHA-256 used for the persisted SMS logical ID shared with the Go producer.
/// Kept in the SMS core so the archive cutover does not add a network-fetched
/// crypto dependency merely to preserve an existing on-disk identifier.
pub fn stable_digest_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let bit_length = u64::try_from(input.len())
        .unwrap_or(u64::MAX / 8)
        .saturating_mul(8);
    let mut padded = Vec::with_capacity(input.len().saturating_add(72));
    padded.extend_from_slice(input);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    let (blocks, remainder) = padded.as_chunks::<64>();
    debug_assert!(remainder.is_empty());
    for block in blocks {
        let mut words = [0_u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);
        }
        for index in 16..64 {
            let small0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let small1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(small0)
                .wrapping_add(words[index - 7])
                .wrapping_add(small1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let choice = (e & f) ^ (!e & g);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let large0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let large1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let first = h
                .wrapping_add(large1)
                .wrapping_add(choice)
                .wrapping_add(ROUND[index])
                .wrapping_add(words[index]);
            let second = large0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut encoded = String::with_capacity(64);
    for value in state {
        use std::fmt::Write;
        write!(&mut encoded, "{value:08x}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_go_mobile_terminated_fixture() {
        let data = [
            0x01, 0x2a, 0x04, 0x91, 0x44, 0x77, 0xf6, 0x00, 0x03, 0x01, 0x02, 0x03,
        ];
        let parsed = MobileTerminatedData::parse(&data).unwrap();
        assert_eq!(parsed.reference, 0x2a);
        assert_eq!(parsed.originator.as_deref(), Some("+44776"));
        assert_eq!(parsed.destination, None);
        assert_eq!(parsed.tpdu, [1, 2, 3]);
    }

    #[test]
    fn builds_go_compatible_mobile_originated_fixture() {
        let encoded = build_mobile_originated_data(0x2a, "+447802002606", &[1, 2, 3]).unwrap();
        assert_eq!(
            encoded,
            [
                0x00, 0x2a, 0x00, 0x07, 0x91, 0x44, 0x87, 0x20, 0x00, 0x62, 0x60, 0x03, 1, 2, 3
            ]
        );
    }

    #[test]
    fn formatted_smsc_preserves_existing_normalization() {
        let expected = build_mobile_originated_data(42, "+447802002606", &[1, 2, 3]).unwrap();
        assert_eq!(
            build_mobile_originated_data(42, "  +44 (7802) 002-606  ", &[1, 2, 3]).unwrap(),
            expected
        );
        assert!(build_mobile_originated_data(42, "unavailable", &[1]).is_err());
    }

    #[test]
    fn rejects_truncation_trailing_data_and_invalid_bcd() {
        for data in [
            vec![],
            vec![0x01, 1, 4, 0x91],
            vec![0x01, 1, 2, 0x91, 0xfa, 0, 1, 1],
            vec![0x01, 1, 0, 0, 1, 1, 0],
        ] {
            assert!(
                MobileTerminatedData::parse(&data).is_err(),
                "accepted {data:02x?}"
            );
        }
    }

    #[test]
    fn delivery_reports_are_strict_and_correlated_once() {
        assert_eq!(
            DeliveryReport::parse(&[0x03, 7]).unwrap(),
            DeliveryReport::Ack { reference: 7 }
        );
        assert_eq!(
            DeliveryReport::parse(&[0x05, 8, 1, 0x95]).unwrap(),
            DeliveryReport::Error {
                reference: 8,
                cause: 0x15
            }
        );
        assert_eq!(
            DeliveryReport::parse(&[0x03, 7, 2, 0xaa, 0xbb]).unwrap(),
            DeliveryReport::Ack { reference: 7 }
        );
        assert!(DeliveryReport::parse(&[0x03, 7, 2, 0xaa]).is_err());
        let mut tracker = DeliveryTracker::default();
        tracker
            .insert(
                7,
                PendingDelivery {
                    message_id: "message-1".into(),
                    part: 0,
                },
            )
            .unwrap();
        assert_eq!(
            tracker
                .complete(DeliveryReport::Ack { reference: 7 })
                .unwrap()
                .message_id,
            "message-1"
        );
        assert_eq!(
            tracker
                .complete(DeliveryReport::Ack { reference: 7 })
                .unwrap_err(),
            SmsError::UnknownDeliveryReference
        );
    }

    #[test]
    fn pending_reference_cannot_be_reused() {
        let mut tracker = DeliveryTracker::default();
        let delivery = PendingDelivery {
            message_id: "message-1".into(),
            part: 0,
        };
        tracker.insert(1, delivery.clone()).unwrap();
        assert_eq!(
            tracker.insert(1, delivery).unwrap_err(),
            SmsError::DuplicatePendingReference
        );
        assert_eq!(tracker.pending_len(), 1);
    }

    #[test]
    fn stable_digest_matches_sha256_and_go_archive_key() {
        assert_eq!(
            stable_digest_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            stable_digest_hex(b"reader\x00888\x00BALANCE\x002026-09-01T12:03:52.123456789Z"),
            "2c513a5bd1137e6b4aaef5c126835b037292828d1bbc42e593f8e5d301e6ea69"
        );
    }
}
