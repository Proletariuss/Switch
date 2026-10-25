//! SMS-DELIVER framing. Timestamp bytes are retained for explicit calendar conversion.
use crate::{address, udh, user_data};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub sender: String,
    pub timestamp: [u8; 7],
    pub service_center_time: Option<crate::timestamp::Timestamp>,
    pub protocol_id: u8,
    pub dcs: u8,
    pub concatenation: Option<udh::Concatenation>,
    pub udh_elements: Vec<(u8, Vec<u8>)>,
    pub content: user_data::Content,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Framing,
    Address,
    Content(user_data::DecodeError),
}

pub fn parse(tpdu: &[u8]) -> Result<Message, Error> {
    if tpdu.len() < 3 || tpdu.len() > 232 || tpdu[0] & 3 != 0 {
        return Err(Error::Framing);
    }
    let (sender, offset) = address::parse(tpdu, 1).map_err(|error| match error {
        address::Error::Framing => Error::Framing,
        address::Error::Invalid => Error::Address,
    })?;
    let fixed = tpdu.get(offset..offset + 10).ok_or(Error::Framing)?;
    let timestamp = fixed[2..9].try_into().map_err(|_| Error::Framing)?;
    let service_center_time = crate::timestamp::parse(&fixed[2..9]).ok();
    let data = &tpdu[offset + 10..];
    let has_header = tpdu[0] & 0x40 != 0;
    let parsed_header = if has_header {
        Some(udh::parse(data).map_err(|_| Error::Framing)?)
    } else {
        None
    };
    let concatenation = parsed_header
        .as_ref()
        .and_then(|header| header.concatenation);
    let udh_elements = parsed_header
        .map(|header| {
            header
                .elements
                .into_iter()
                .map(|(id, value)| (id, value.to_vec()))
                .collect()
        })
        .unwrap_or_default();
    let content =
        user_data::decode(fixed[1], fixed[9], has_header, data).map_err(Error::Content)?;
    Ok(Message {
        sender,
        timestamp,
        service_center_time,
        protocol_id: fixed[0],
        dcs: fixed[1],
        concatenation,
        udh_elements,
        content,
    })
}

/// Parse a modem-storage TPDU whose fixed-size slot may include zero padding.
/// The SMS-DELIVER user-data length is authoritative; only bytes beyond that
/// declared payload are discarded. GSM-7 unused spare bits are normalized to
/// match the carrier modem decoder before strict parsing.
pub fn parse_stored(tpdu: &[u8]) -> Result<Message, Error> {
    if tpdu.len() < 3 || tpdu.len() > 232 || tpdu[0] & 3 != 0 {
        return Err(Error::Framing);
    }
    let (_, offset) = address::parse(tpdu, 1).map_err(|error| match error {
        address::Error::Framing => Error::Framing,
        address::Error::Invalid => Error::Address,
    })?;
    let fixed = tpdu.get(offset..offset + 10).ok_or(Error::Framing)?;
    let coding = crate::user_data::coding(fixed[1]);
    let udl = usize::from(fixed[9]);
    let data_bytes = match coding {
        crate::user_data::Coding::Gsm7 => (udl * 7).div_ceil(8),
        crate::user_data::Coding::Binary | crate::user_data::Coding::Utf16 => udl,
        crate::user_data::Coding::Unsupported => {
            return Err(Error::Content(
                crate::user_data::DecodeError::UnsupportedCoding,
            ));
        }
    };
    if data_bytes > 140 {
        return Err(Error::Content(crate::user_data::DecodeError::InvalidLength));
    }
    let data_start = offset.checked_add(10).ok_or(Error::Framing)?;
    let end = data_start.checked_add(data_bytes).ok_or(Error::Framing)?;
    if end > tpdu.len() {
        return Err(Error::Framing);
    }
    let mut declared = tpdu[..end].to_vec();
    if coding == crate::user_data::Coding::Gsm7 && data_bytes > 0 {
        let used_bits = (udl * 7) % 8;
        if used_bits != 0 {
            declared[end - 1] &= (1_u8 << used_bits) - 1;
        }
    }
    parse(&declared)
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURE: &[u8] = &[
        0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x41,
    ];
    #[test]
    fn parses_sender_and_body_without_smsc_prefix() {
        let message = parse(FIXTURE).unwrap();
        assert_eq!(message.sender, "888");
        assert_eq!(message.content, user_data::Content::Text("A".into()));
        assert_eq!(message.timestamp, [0x62, 0x90, 0x60, 0, 0, 0, 0]);
        assert_eq!(message.service_center_time.unwrap().year, 2026);
    }

    #[test]
    fn invalid_calendar_does_not_discard_message_content() {
        let mut fixture = FIXTURE.to_vec();
        fixture[8] = 0x31;
        let message = parse(&fixture).unwrap();
        assert!(message.service_center_time.is_none());
        assert_eq!(message.timestamp[1], 0x31);
        assert_eq!(message.content, user_data::Content::Text("A".into()));
    }

    #[test]
    fn alphanumeric_sender_length_uses_semi_octets() {
        let address = crate::septets::pack(b"OKX", 0).unwrap();
        let mut tpdu = vec![0, 6, 0xd0];
        tpdu.extend(address);
        tpdu.extend_from_slice(&FIXTURE[5..]);
        let message = parse(&tpdu).unwrap();
        assert_eq!(message.sender, "OKX");
        assert_eq!(message.content, user_data::Content::Text("A".into()));
    }
    #[test]
    fn rejects_all_truncations_and_invalid_numeric_address() {
        for count in 0..FIXTURE.len() {
            assert!(parse(&FIXTURE[..count]).is_err());
        }
        let mut malformed = FIXTURE.to_vec();
        malformed[3] = 0xaa;
        assert_eq!(parse(&malformed), Err(Error::Address));
        let mut trailing = FIXTURE.to_vec();
        trailing.push(0);
        assert!(parse(&trailing).is_err());
    }

    #[test]
    fn stored_parser_ignores_fixed_slot_padding_and_normalizes_gsm7_spare_bits() {
        let mut padded = FIXTURE.to_vec();
        padded.extend_from_slice(&[0; 16]);
        assert!(parse(&padded).is_err());
        assert_eq!(
            parse_stored(&padded).unwrap().content,
            user_data::Content::Text("A".into())
        );

        let mut spare_bits = vec![
            0x04, 0x03, 0x81, 0x01, 0xf1, 0x00, 0x00, 0x62, 0x50, 0x72, 0x41, 0x90, 0x41, 0x0a,
            0x37, 0x54, 0x74, 0x7a, 0x0e, 0x4a, 0xbb, 0xcd, 0x6f, 0x79, 0x3b, 0x4c, 0x4f, 0xbf,
            0xdd, 0xa0, 0xf4, 0x1c, 0xe4, 0x7e, 0xd3, 0x41, 0x61, 0x7b, 0x38, 0xcd, 0x0e, 0x8b,
            0xd9, 0x65, 0x90, 0xf9, 0x2d, 0x07, 0xe5, 0xdf, 0x75, 0x39, 0x28, 0x3c, 0x1e, 0xbf,
            0xeb, 0x6e, 0x3a, 0x88, 0x9e, 0x87, 0x97, 0x1b,
        ];
        let content = parse_stored(&spare_bits).unwrap().content;
        assert_eq!(
            content,
            user_data::Content::Text(
                "This information is not available for your account type".into()
            )
        );
        spare_bits.push(0);
        assert_eq!(
            parse_stored(&spare_bits).unwrap().content,
            user_data::Content::Text(
                "This information is not available for your account type".into()
            )
        );
    }
}
