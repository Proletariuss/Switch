#![forbid(unsafe_code)]

use std::fmt;

pub const USIM_AID_PREFIX: &[u8] = &[0xA0, 0x00, 0x00, 0x00, 0x87, 0x10, 0x02];
pub const ISIM_AID_PREFIX: &[u8] = &[0xA0, 0x00, 0x00, 0x00, 0x87, 0x10, 0x04];
pub const SIM_PROTOCOL_CONTRACT: &str = "3gpp-aka-apdu-v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SimProtocolError {
    InvalidChallengeLength,
    InvalidAid,
    InvalidCommandLength,
    ResponseTooShort,
    CardStatus {
        sw1: u8,
        sw2: u8,
    },
    UnexpectedAkaTag(u8),
    MissingField(&'static str),
    InvalidFieldLength {
        field: &'static str,
        expected: &'static str,
        actual: usize,
    },
    InvalidBcdDigit(u8),
    TrailingData,
}

impl fmt::Display for SimProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChallengeLength => {
                formatter.write_str("AKA RAND and AUTN must each contain 16 bytes")
            }
            Self::InvalidAid => formatter.write_str("application AID must contain 5 to 16 bytes"),
            Self::InvalidCommandLength => formatter.write_str("invalid short APDU command length"),
            Self::ResponseTooShort => formatter.write_str("APDU response is missing status words"),
            Self::CardStatus { sw1, sw2 } => write!(formatter, "card returned {sw1:02X}{sw2:02X}"),
            Self::UnexpectedAkaTag(tag) => {
                write!(formatter, "unexpected AKA response tag {tag:02X}")
            }
            Self::MissingField(field) => write!(formatter, "AKA response is missing {field}"),
            Self::InvalidFieldLength {
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "invalid {field} length {actual}; expected {expected}"
            ),
            Self::TrailingData => formatter.write_str("AKA response contains trailing data"),
            Self::InvalidBcdDigit(digit) => write!(formatter, "invalid BCD digit {digit:x}"),
        }
    }
}

impl std::error::Error for SimProtocolError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApduResponse {
    pub data: Vec<u8>,
    pub sw1: u8,
    pub sw2: u8,
}

pub trait ApduTransport {
    type Error;

    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error>;
}

#[derive(Debug)]
pub enum ApduExchangeError<E> {
    Transport(E),
    Protocol(SimProtocolError),
    TooManyFollowUps,
    ResponseTooLarge,
}

impl<E: fmt::Display> fmt::Display for ApduExchangeError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "APDU transport failed: {error}"),
            Self::Protocol(error) => write!(formatter, "APDU protocol failed: {error}"),
            Self::TooManyFollowUps => formatter.write_str("APDU GET RESPONSE limit exceeded"),
            Self::ResponseTooLarge => formatter.write_str("APDU response size limit exceeded"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for ApduExchangeError<E> {}

/// Executes a short APDU and follows ISO 7816 `61xx` responses with bounded
/// GET RESPONSE commands. The bound prevents a faulty card from creating an
/// infinite loop or unbounded secret-bearing buffer.
pub fn exchange_apdu<T: ApduTransport>(
    transport: &mut T,
    command: &[u8],
) -> Result<ApduResponse, ApduExchangeError<T::Error>> {
    const MAX_FOLLOW_UPS: usize = 8;
    const MAX_RESPONSE_BYTES: usize = 4_096;

    short_le_index(command).map_err(ApduExchangeError::Protocol)?;
    let mut current = command.to_vec();
    let mut data = Vec::new();
    for follow_ups in 0..=MAX_FOLLOW_UPS {
        let raw = transport
            .transmit(&current)
            .map_err(ApduExchangeError::Transport)?;
        if raw.len() > MAX_RESPONSE_BYTES + 2 {
            return Err(ApduExchangeError::ResponseTooLarge);
        }
        let mut response = ApduResponse::parse(&raw).map_err(ApduExchangeError::Protocol)?;
        if response.sw1 == 0x6c {
            // Only modify an actual Le field. Case-3 data (including an AKA
            // challenge) must never be silently altered and retransmitted.
            if let Some(index) = short_le_index(&current).map_err(ApduExchangeError::Protocol)? {
                if follow_ups == MAX_FOLLOW_UPS {
                    return Err(ApduExchangeError::TooManyFollowUps);
                }
                current[index] = response.sw2;
                continue;
            }
            return Ok(response);
        }
        if data.len().saturating_add(response.data.len()) > MAX_RESPONSE_BYTES {
            return Err(ApduExchangeError::ResponseTooLarge);
        }
        data.extend_from_slice(&response.data);
        if response.sw1 != 0x61 {
            response.data = data;
            return Ok(response);
        }
        if follow_ups == MAX_FOLLOW_UPS {
            return Err(ApduExchangeError::TooManyFollowUps);
        }
        // Retain the original logical channel, but not command chaining.
        current = vec![command[0] & !0x10, 0xc0, 0, 0, response.sw2];
    }
    Err(ApduExchangeError::TooManyFollowUps)
}

fn short_le_index(command: &[u8]) -> Result<Option<usize>, SimProtocolError> {
    match command.len() {
        4 => Ok(None),
        5 => Ok(Some(4)),
        6..=261 if command[4] != 0 => {
            let data_end = 5 + usize::from(command[4]);
            if command.len() == data_end {
                Ok(None)
            } else if command.len() == data_end + 1 {
                Ok(Some(data_end))
            } else {
                Err(SimProtocolError::InvalidCommandLength)
            }
        }
        _ => Err(SimProtocolError::InvalidCommandLength),
    }
}

impl ApduResponse {
    pub fn parse(raw: &[u8]) -> Result<Self, SimProtocolError> {
        if raw.len() < 2 {
            return Err(SimProtocolError::ResponseTooShort);
        }
        let split = raw.len() - 2;
        Ok(Self {
            data: raw[..split].to_vec(),
            sw1: raw[split],
            sw2: raw[split + 1],
        })
    }

    pub fn success_data(&self) -> Result<&[u8], SimProtocolError> {
        if (self.sw1, self.sw2) == (0x90, 0x00) {
            Ok(&self.data)
        } else {
            Err(SimProtocolError::CardStatus {
                sw1: self.sw1,
                sw2: self.sw2,
            })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AkaResult {
    Success {
        res: Vec<u8>,
        ck: [u8; 16],
        ik: [u8; 16],
    },
    SynchronizationFailure {
        auts: [u8; 14],
    },
    AuthenticationReject,
}

pub fn select_application_apdu(aid: &[u8]) -> Result<Vec<u8>, SimProtocolError> {
    if !(5..=16).contains(&aid.len()) {
        return Err(SimProtocolError::InvalidAid);
    }
    let mut apdu = Vec::with_capacity(5 + aid.len());
    apdu.extend_from_slice(&[0x00, 0xA4, 0x04, 0x04, aid.len() as u8]);
    apdu.extend_from_slice(aid);
    Ok(apdu)
}

/// Build ISO 7816 SELECT FILE for a transparent EF. This is intentionally
/// separate from application selection so callers can keep the card context
/// and read only the requested file.
pub fn select_file_apdu(file_id: [u8; 2]) -> Vec<u8> {
    vec![0x00, 0xA4, 0x00, 0x04, 0x02, file_id[0], file_id[1], 0x00]
}

/// Build a bounded READ BINARY command. EF reads are capped to one short-APDU
/// response; larger files must be read in explicit chunks by the caller.
pub fn read_binary_apdu(offset: u16, length: u8) -> Vec<u8> {
    vec![0x00, 0xB0, (offset >> 8) as u8, offset as u8, length]
}

/// Decode EF-ICCID's swapped-nibble BCD representation (ISO 7816-4).
/// Padding nibble `F` is accepted only as the final nibble.
pub fn decode_iccid(raw: &[u8]) -> Result<String, SimProtocolError> {
    if !(5..=10).contains(&raw.len()) {
        return Err(SimProtocolError::InvalidFieldLength {
            field: "ICCID",
            expected: "5..=10 bytes",
            actual: raw.len(),
        });
    }
    let mut digits = String::with_capacity(raw.len() * 2);
    for (index, byte) in raw.iter().copied().enumerate() {
        for nibble in [byte & 0x0f, byte >> 4] {
            if nibble == 0x0f
                && index == raw.len() - 1
                && (byte & 0x0f) <= 9
                && nibble == (byte >> 4)
            {
                return Ok(digits);
            }
            if nibble > 9 {
                return Err(SimProtocolError::InvalidBcdDigit(nibble));
            }
            digits.push(char::from(b'0' + nibble));
        }
    }
    Ok(digits)
}

/// Decode EF-IMSI (TS 31.102): one length byte followed by swapped BCD. The
/// first decoded nibble is the parity/type marker and is never part of IMSI.
pub fn decode_imsi(raw: &[u8]) -> Result<String, SimProtocolError> {
    let Some(&length) = raw.first() else {
        return Err(SimProtocolError::MissingField("IMSI length"));
    };
    let length = usize::from(length);
    if length == 0 || raw.len() != length + 1 {
        return Err(SimProtocolError::InvalidFieldLength {
            field: "IMSI",
            expected: "length-prefixed BCD",
            actual: raw.len(),
        });
    }
    let mut digits = decode_swapped_bcd(&raw[1..=length])?;
    if digits.is_empty() || digits.remove(0) != '9' {
        return Err(SimProtocolError::InvalidBcdDigit(0x0f));
    }
    if digits.len() < 6 || digits.len() > 15 {
        return Err(SimProtocolError::InvalidFieldLength {
            field: "IMSI digits",
            expected: "6..=15 digits",
            actual: digits.len(),
        });
    }
    Ok(digits)
}

pub fn decode_mnc_length(raw: &[u8]) -> Result<usize, SimProtocolError> {
    if raw.len() < 4 {
        return Err(SimProtocolError::InvalidFieldLength {
            field: "EF-AD",
            expected: "at least 4 bytes",
            actual: raw.len(),
        });
    }
    match raw[3] & 0x0f {
        2 => Ok(2),
        3 => Ok(3),
        value => Err(SimProtocolError::InvalidBcdDigit(value)),
    }
}

/// Decode the 14-byte ADN footer used by EF-MSISDN. Extension records are
/// rejected so a truncated subscriber number is never published.
pub fn decode_msisdn_record(record: &[u8]) -> Result<Option<String>, SimProtocolError> {
    if record.len() < 14 {
        return Err(SimProtocolError::InvalidFieldLength {
            field: "MSISDN record",
            expected: "at least 14 bytes",
            actual: record.len(),
        });
    }
    let footer = &record[record.len() - 14..];
    let length = usize::from(footer[0]);
    if length == 0 || footer[0] == 0xff || length > 11 || footer[13] != 0xff {
        return Ok(None);
    }
    let bcd_length = length.saturating_sub(1);
    if bcd_length == 0 || bcd_length > 10 {
        return Ok(None);
    }
    let digits = decode_swapped_bcd(&footer[2..2 + bcd_length])?;
    if digits.is_empty() || digits.bytes().all(|digit| digit == b'0') {
        return Ok(None);
    }
    Ok(Some(if footer[1] & 0x70 == 0x10 {
        format!("+{digits}")
    } else {
        digits
    }))
}

/// Decode the service-centre address stored in the fixed 12-byte SCA field at
/// offset `record_length - 15` of EF-SMSP.
pub fn decode_smsp_record(record: &[u8]) -> Result<Option<String>, SimProtocolError> {
    if record.len() < 28 {
        return Err(SimProtocolError::InvalidFieldLength {
            field: "SMSP record",
            expected: "at least 28 bytes",
            actual: record.len(),
        });
    }
    let offset = record.len() - 15;
    let sca = &record[offset..offset + 12];
    let length = usize::from(sca[0]);
    if length <= 1 || length + 1 > sca.len() || sca[1] != 0x91 {
        return Ok(None);
    }
    let digits = decode_swapped_bcd(&sca[2..=length])?;
    if digits.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!("+{digits}")))
    }
}

fn decode_swapped_bcd(raw: &[u8]) -> Result<String, SimProtocolError> {
    let mut digits = String::with_capacity(raw.len() * 2);
    for (index, byte) in raw.iter().copied().enumerate() {
        for nibble in [byte & 0x0f, byte >> 4] {
            if nibble == 0x0f && index == raw.len() - 1 {
                return Ok(digits);
            }
            if nibble > 9 {
                return Err(SimProtocolError::InvalidBcdDigit(nibble));
            }
            digits.push(char::from(b'0' + nibble));
        }
    }
    Ok(digits)
}

pub fn authenticate_3g_apdu(rand: &[u8], autn: &[u8]) -> Result<Vec<u8>, SimProtocolError> {
    if rand.len() != 16 || autn.len() != 16 {
        return Err(SimProtocolError::InvalidChallengeLength);
    }
    let mut apdu = Vec::with_capacity(39);
    apdu.extend_from_slice(&[0x00, 0x88, 0x00, 0x81, 34, 16]);
    apdu.extend_from_slice(rand);
    apdu.push(16);
    apdu.extend_from_slice(autn);
    Ok(apdu)
}

pub fn parse_authenticate_3g(raw_apdu_response: &[u8]) -> Result<AkaResult, SimProtocolError> {
    let response = ApduResponse::parse(raw_apdu_response)?;
    parse_authenticate_payload(response.success_data()?)
}

pub fn parse_authenticate_payload(payload: &[u8]) -> Result<AkaResult, SimProtocolError> {
    let Some((&tag, fields)) = payload.split_first() else {
        return Err(SimProtocolError::MissingField("AKA tag"));
    };
    match tag {
        0xDB => {
            let (res, fields) = read_field(fields, "RES")?;
            if !(4..=16).contains(&res.len()) {
                return Err(SimProtocolError::InvalidFieldLength {
                    field: "RES",
                    expected: "4..=16",
                    actual: res.len(),
                });
            }
            let (ck, fields) = read_fixed_field::<16>(fields, "CK")?;
            let (ik, fields) = read_fixed_field::<16>(fields, "IK")?;
            // TS 31.102 permits a USIM to append the derived 8-byte GSM Kc
            // after RES/CK/IK.  It is not used by EAP-AKA, but rejecting it
            // breaks otherwise valid cards (including Orange/CTExcel USIMs).
            // Accept only the precisely length-delimited Kc form so arbitrary
            // trailing secret-bearing data is still rejected.
            let fields = if fields.is_empty() {
                fields
            } else {
                let (_, trailing) = read_fixed_field::<8>(fields, "Kc")?;
                trailing
            };
            if !fields.is_empty() {
                return Err(SimProtocolError::TrailingData);
            }
            Ok(AkaResult::Success {
                res: res.to_vec(),
                ck,
                ik,
            })
        }
        0xDC => {
            if fields.is_empty() || fields == [0] {
                return Ok(AkaResult::AuthenticationReject);
            }
            let (auts, trailing) = read_fixed_field::<14>(fields, "AUTS")?;
            if !trailing.is_empty() {
                return Err(SimProtocolError::TrailingData);
            }
            Ok(AkaResult::SynchronizationFailure { auts })
        }
        0xDD if fields.is_empty() => Ok(AkaResult::AuthenticationReject),
        0xDD => Err(SimProtocolError::TrailingData),
        other => Err(SimProtocolError::UnexpectedAkaTag(other)),
    }
}

fn read_field<'a>(
    input: &'a [u8],
    field: &'static str,
) -> Result<(&'a [u8], &'a [u8]), SimProtocolError> {
    let Some((&length, payload)) = input.split_first() else {
        return Err(SimProtocolError::MissingField(field));
    };
    let length = usize::from(length);
    if payload.len() < length {
        return Err(SimProtocolError::InvalidFieldLength {
            field,
            expected: "declared length",
            actual: payload.len(),
        });
    }
    Ok(payload.split_at(length))
}

fn read_fixed_field<'a, const LENGTH: usize>(
    input: &'a [u8],
    field: &'static str,
) -> Result<([u8; LENGTH], &'a [u8]), SimProtocolError> {
    let (value, trailing) = read_field(input, field)?;
    let value = value
        .try_into()
        .map_err(|_| SimProtocolError::InvalidFieldLength {
            field,
            expected: match LENGTH {
                8 => "8",
                14 => "14",
                16 => "16",
                _ => "fixed",
            },
            actual: value.len(),
        })?;
    Ok((value, trailing))
}

#[cfg(test)]
mod tests {
    use super::{
        AkaResult, ApduExchangeError, ApduResponse, ApduTransport, ISIM_AID_PREFIX,
        SimProtocolError, authenticate_3g_apdu, decode_iccid, decode_imsi, decode_mnc_length,
        decode_msisdn_record, decode_smsp_record, exchange_apdu, parse_authenticate_3g,
        parse_authenticate_payload, read_binary_apdu, select_application_apdu, select_file_apdu,
    };

    #[test]
    fn decodes_imsi_and_mnc_length() {
        let raw = [0x08, 0x29, 0x43, 0x01, 0x80, 0x69, 0x93, 0x92, 0x68];
        assert_eq!(decode_imsi(&raw).unwrap(), "234100896392986");
        assert_eq!(decode_mnc_length(&[0, 0, 0, 0x03]).unwrap(), 3);
        assert!(decode_mnc_length(&[0, 0, 0, 0x01]).is_err());
        assert!(
            decode_imsi(&[0x08, 0x29, 0x43, 0x01, 0x80, 0x69, 0x93, 0x92, 0x68, 0x00]).is_err()
        );
    }

    #[test]
    fn decodes_msisdn_and_smsp_records() {
        let mut msisdn = vec![0xff; 32];
        let start = msisdn.len() - 14;
        msisdn[start..].copy_from_slice(&[
            7, 0x91, 0x44, 0x87, 0x28, 0x59, 0x70, 0x13, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ]);
        assert_eq!(
            decode_msisdn_record(&msisdn).unwrap(),
            Some("+447882950731".to_owned())
        );
        let mut smsp = vec![0xff; 28];
        smsp[13..25].copy_from_slice(&[7, 0x91, 0x44, 0x87, 0x28, 0x59, 0x70, 0x13, 0, 0, 0, 0]);
        assert_eq!(
            decode_smsp_record(&smsp).unwrap(),
            Some("+447882950731".to_owned())
        );
    }

    #[test]
    fn encodes_complete_application_aid_and_aka_challenge() {
        let aid = [ISIM_AID_PREFIX, &[0xFF, 0x01]].concat();
        let select = select_application_apdu(&aid).expect("SELECT by AID");
        assert_eq!(&select[..5], &[0x00, 0xA4, 0x04, 0x04, 9]);
        assert_eq!(&select[5..], aid);

        let rand = [0x11; 16];
        let autn = [0x22; 16];
        let authenticate = authenticate_3g_apdu(&rand, &autn).expect("AUTHENTICATE");
        assert_eq!(&authenticate[..6], &[0x00, 0x88, 0x00, 0x81, 34, 16]);
        assert_eq!(&authenticate[6..22], rand);
        assert_eq!(authenticate[22], 16);
        assert_eq!(&authenticate[23..], autn);
    }

    #[test]
    fn rejects_partial_challenge_and_prefix_only_aid() {
        assert_eq!(
            authenticate_3g_apdu(&[0; 15], &[0; 16]),
            Err(SimProtocolError::InvalidChallengeLength)
        );
        assert_eq!(
            select_application_apdu(&[0; 4]),
            Err(SimProtocolError::InvalidAid)
        );
    }

    #[test]
    fn decodes_success_sync_failure_and_reject() {
        let mut success = vec![0xDB, 8];
        success.extend_from_slice(&[1; 8]);
        success.push(16);
        success.extend_from_slice(&[2; 16]);
        success.push(16);
        success.extend_from_slice(&[3; 16]);
        success.extend_from_slice(&[0x90, 0x00]);
        assert!(matches!(
            parse_authenticate_3g(&success),
            Ok(AkaResult::Success { res, ck, ik })
                if res == vec![1; 8] && ck == [2; 16] && ik == [3; 16]
        ));

        let mut sync = vec![0xDC, 14];
        sync.extend_from_slice(&[4; 14]);
        assert_eq!(
            parse_authenticate_payload(&sync),
            Ok(AkaResult::SynchronizationFailure { auts: [4; 14] })
        );
        assert_eq!(
            parse_authenticate_payload(&[0xDC, 0]),
            Ok(AkaResult::AuthenticationReject)
        );
        assert_eq!(
            parse_authenticate_payload(&[0xDD]),
            Ok(AkaResult::AuthenticationReject)
        );
    }

    #[test]
    fn decodes_success_with_optional_usim_gsm_kc() {
        let mut success = vec![0xDB, 8];
        success.extend_from_slice(&[1; 8]);
        success.push(16);
        success.extend_from_slice(&[2; 16]);
        success.push(16);
        success.extend_from_slice(&[3; 16]);
        success.push(8);
        success.extend_from_slice(&[4; 8]);

        assert!(matches!(
            parse_authenticate_payload(&success),
            Ok(AkaResult::Success { res, ck, ik })
                if res == vec![1; 8] && ck == [2; 16] && ik == [3; 16]
        ));
    }

    #[test]
    fn rejects_bad_status_lengths_and_trailing_secrets() {
        assert_eq!(
            ApduResponse::parse(&[0x90]),
            Err(SimProtocolError::ResponseTooShort)
        );
        assert_eq!(
            parse_authenticate_3g(&[0xDB, 0x69, 0x85]),
            Err(SimProtocolError::CardStatus {
                sw1: 0x69,
                sw2: 0x85
            })
        );
        let mut malformed = vec![0xDB, 8];
        malformed.extend_from_slice(&[1; 8]);
        malformed.push(15);
        malformed.extend_from_slice(&[2; 15]);
        assert!(matches!(
            parse_authenticate_payload(&malformed),
            Err(SimProtocolError::InvalidFieldLength { field: "CK", .. })
        ));

        let mut trailing = vec![0xDC, 14];
        trailing.extend_from_slice(&[4; 14]);
        trailing.push(0xAA);
        assert_eq!(
            parse_authenticate_payload(&trailing),
            Err(SimProtocolError::TrailingData)
        );
    }

    #[derive(Default)]
    struct ScriptedTransport {
        commands: Vec<Vec<u8>>,
        responses: std::collections::VecDeque<Vec<u8>>,
    }

    impl ApduTransport for ScriptedTransport {
        type Error = &'static str;

        fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
            self.commands.push(command.to_vec());
            self.responses.pop_front().ok_or("missing response")
        }
    }

    #[test]
    fn corrects_get_response_length_without_losing_prior_data() {
        let mut transport = ScriptedTransport {
            responses: [vec![1, 0x61, 4], vec![0x6c, 2], vec![2, 3, 0x90, 0]]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let result = exchange_apdu(&mut transport, &[3, 0xa4, 0, 0]).unwrap();
        assert_eq!(result.data, [1, 2, 3]);
        assert_eq!(transport.commands[2], [3, 0xc0, 0, 0, 2]);
    }

    #[test]
    fn length_correction_never_overwrites_command_data() {
        let mut transport = ScriptedTransport {
            responses: [vec![0x6c, 9]].into_iter().collect(),
            ..Default::default()
        };
        let result = exchange_apdu(&mut transport, &[0, 0x88, 0, 0x81, 2, 1, 2]).unwrap();
        assert_eq!(result.sw1, 0x6c);
        assert_eq!(transport.commands.len(), 1);
        let mut looping = ScriptedTransport {
            responses: std::iter::repeat_n(vec![0x6c, 1], 9).collect(),
            ..Default::default()
        };
        assert!(matches!(
            exchange_apdu(&mut looping, &[0, 0xb0, 0, 0, 0]),
            Err(ApduExchangeError::TooManyFollowUps)
        ));
        assert_eq!(looping.commands.len(), 9);
    }

    #[test]
    fn get_response_retains_logical_channel() {
        for cla in [0x01, 0x03, 0x40, 0x4f] {
            let mut transport = ScriptedTransport {
                responses: [vec![0x61, 0], vec![0x90, 0]].into_iter().collect(),
                ..Default::default()
            };
            exchange_apdu(&mut transport, &[cla, 0xa4, 0, 0]).unwrap();
            assert_eq!(transport.commands[1], [cla, 0xc0, 0, 0, 0]);
        }
    }

    #[test]
    fn rejects_oversized_initial_response_and_invalid_command() {
        let mut transport = ScriptedTransport {
            responses: [vec![0; 4099]].into_iter().collect(),
            ..Default::default()
        };
        assert!(matches!(
            exchange_apdu(&mut transport, &[0, 0xa4, 0, 0]),
            Err(ApduExchangeError::ResponseTooLarge)
        ));
        let before = transport.commands.len();
        assert!(matches!(
            exchange_apdu(&mut transport, &[]),
            Err(ApduExchangeError::Protocol(
                SimProtocolError::InvalidCommandLength
            ))
        ));
        assert_eq!(transport.commands.len(), before);
    }

    #[test]
    fn follows_get_response_with_strict_bounds() {
        let mut transport = ScriptedTransport {
            responses: [vec![1, 2, 0x61, 0x03], vec![3, 4, 5, 0x90, 0x00]]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let response =
            exchange_apdu(&mut transport, &[0x00, 0xA4, 0x00, 0x00]).expect("bounded GET RESPONSE");
        assert_eq!(response.data, [1, 2, 3, 4, 5]);
        assert_eq!(transport.commands[1], [0x00, 0xC0, 0x00, 0x00, 0x03]);

        let mut looping = ScriptedTransport {
            responses: std::iter::repeat_n(vec![0x61, 0x01], 9).collect(),
            ..Default::default()
        };
        assert!(matches!(
            exchange_apdu(&mut looping, &[0x00, 0xa4, 0, 0]),
            Err(ApduExchangeError::TooManyFollowUps)
        ));
    }

    #[test]
    fn builds_read_only_identity_commands() {
        assert_eq!(
            select_file_apdu([0x6f, 0x07]),
            [0, 0xa4, 0, 4, 2, 0x6f, 7, 0]
        );
        assert_eq!(read_binary_apdu(0x0123, 10), [0, 0xb0, 1, 0x23, 10]);
    }

    #[test]
    fn decodes_swapped_iccid_bcd_and_padding() {
        assert_eq!(
            decode_iccid(&[0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xF6]).unwrap(),
            "8901234567890123456"
        );
        assert_eq!(
            decode_iccid(&[0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54]).unwrap(),
            "890123456789012345"
        );
        assert!(matches!(
            decode_iccid(&[0x9a, 0x10, 0x32, 0x54, 0x76]),
            Err(SimProtocolError::InvalidBcdDigit(0xa))
        ));
        assert!(matches!(
            decode_iccid(&[0x98, 0x10, 0x32, 0x54, 0xff]),
            Err(SimProtocolError::InvalidBcdDigit(0xf))
        ));
    }
}
