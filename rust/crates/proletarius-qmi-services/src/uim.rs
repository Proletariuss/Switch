// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT
// QMI UIM layouts are adapted from Celmux's MIT-licensed qmi/services/uim.rs.
// File access and PLMN selection preserve Switch's current Go QMI behavior.

use crate::client::{QmiServiceClientError, validate_response};
use crate::{SERVICE_UIM, UIM_GET_FILE_ATTRIBUTES, UIM_READ_RECORD, UIM_READ_TRANSPARENT};
use proletarius_qmi_core::{Client, ClientError, OwnedPacket, ServiceClient};
use proletarius_qmi_wire::Tlv;

const SESSION_PRIMARY_GW_PROVISIONING: u8 = 0;
const EF_IMSI: u16 = 0x6f07;
const EF_AD: u16 = 0x6fad;
const EF_OPL: u16 = 0x6fc6;
const MAX_OPL_RECORDS: u16 = 32;
const OPL_RECORD_LENGTH: u16 = 8;
const SIM_PATHS: [&[u8]; 3] = [&[0x00, 0x3f, 0xff, 0x7f], &[0x20, 0x7f], &[]];

pub type UimClientError = QmiServiceClientError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeMccMnc {
    mcc: [u8; 3],
    mnc: [u8; 3],
    mnc_len: u8,
}

impl NativeMccMnc {
    #[must_use]
    pub fn mcc(&self) -> &str {
        std::str::from_utf8(&self.mcc).expect("MCC is validated ASCII")
    }

    #[must_use]
    pub fn mnc(&self) -> &str {
        std::str::from_utf8(&self.mnc[..usize::from(self.mnc_len)]).expect("MNC is validated ASCII")
    }

    #[must_use]
    pub const fn mnc_len(&self) -> usize {
        self.mnc_len as usize
    }
}

/// Narrow UIM client for SIM identity reads used by Switch's QMI path.
///
/// It shares the caller-owned QMI transport and never opens or claims a
/// physical device. The implementation intentionally covers only the file
/// reads needed to resolve the SIM's home MCC/MNC.
#[derive(Clone)]
pub struct UimClient {
    uim: ServiceClient,
}

impl UimClient {
    pub fn open(client: &Client) -> Result<Self, ClientError> {
        Ok(Self {
            uim: ServiceClient::open(client, SERVICE_UIM)?,
        })
    }

    /// Resolve the SIM home PLMN using the same priority as Switch's Go QMI
    /// backend: first exact decimal EF-OPL PLMN, then IMSI plus EF-AD MNC length.
    pub fn get_native_mcc_mnc(&self) -> Result<NativeMccMnc, UimClientError> {
        if let Ok(Some(plmn)) = self.read_native_plmn_from_opl() {
            return Ok(plmn);
        }
        self.read_native_plmn_from_imsi()
    }

    pub fn read_imsi(&self) -> Result<String, UimClientError> {
        let data = self.read_transparent_fallback(EF_IMSI)?;
        parse_imsi_file(&data)
    }

    pub fn close(&self) -> Result<(), ClientError> {
        self.uim.close()
    }

    fn read_native_plmn_from_opl(&self) -> Result<Option<NativeMccMnc>, UimClientError> {
        let layout = self.record_layout(EF_OPL, OPL_RECORD_LENGTH)?;
        let record_size = layout.record_size.max(OPL_RECORD_LENGTH);
        for record in 1..=layout.record_count.min(MAX_OPL_RECORDS) {
            let bytes = if record == 1 {
                if let Some(first) = &layout.first_record {
                    first.clone()
                } else {
                    self.read_record(EF_OPL, &layout.path, record, record_size)?
                }
            } else {
                self.read_record(EF_OPL, &layout.path, record, record_size)?
            };
            let bytes = trim_file_padding(&bytes);
            if bytes.is_empty() {
                break;
            }
            if bytes.len() >= usize::from(OPL_RECORD_LENGTH)
                && let Some(plmn) = decode_opl_plmn(&bytes[..3])
                && let Some(home) = NativeMccMnc::from_plmn(&plmn)
            {
                return Ok(Some(home));
            }
        }
        Ok(None)
    }

    fn read_native_plmn_from_imsi(&self) -> Result<NativeMccMnc, UimClientError> {
        let imsi = self.read_imsi()?;
        let ad = self.read_transparent_fallback(EF_AD).unwrap_or_default();
        native_mcc_mnc_from_imsi(&imsi, &ad)
    }

    fn record_layout(
        &self,
        file_id: u16,
        fallback_length: u16,
    ) -> Result<RecordLayout, UimClientError> {
        for path in SIM_PATHS {
            if let Ok(attributes) = self.file_attributes(file_id, path)
                && attributes.record_size > 0
                && attributes.record_count > 0
            {
                return Ok(RecordLayout {
                    path: path.to_vec(),
                    record_size: attributes.record_size,
                    record_count: attributes.record_count,
                    first_record: None,
                });
            }
        }

        let mut last_error = None;
        for path in SIM_PATHS {
            match self.read_record(file_id, path, 1, fallback_length) {
                Ok(first_record) => {
                    return Ok(RecordLayout {
                        path: path.to_vec(),
                        record_size: fallback_length,
                        record_count: MAX_OPL_RECORDS,
                        first_record: Some(first_record),
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| malformed("EF-OPL record layout")))
    }

    fn file_attributes(&self, file_id: u16, path: &[u8]) -> Result<FileAttributes, UimClientError> {
        let session = session_value(SESSION_PRIMARY_GW_PROVISIONING);
        let file = file_value(file_id, path)?;
        let response = self.uim.request(
            UIM_GET_FILE_ATTRIBUTES,
            &[Tlv::new(0x01, &session), Tlv::new(0x02, &file)],
        )?;
        validate_response(&response, SERVICE_UIM, UIM_GET_FILE_ATTRIBUTES)?;
        parse_file_attributes(&response)
    }

    fn read_record(
        &self,
        file_id: u16,
        path: &[u8],
        record_number: u16,
        record_length: u16,
    ) -> Result<Vec<u8>, UimClientError> {
        let session = session_value(SESSION_PRIMARY_GW_PROVISIONING);
        let file = file_value(file_id, path)?;
        let mut info = [0_u8; 4];
        info[..2].copy_from_slice(&record_number.to_le_bytes());
        info[2..].copy_from_slice(&record_length.to_le_bytes());
        let response = self.uim.request(
            UIM_READ_RECORD,
            &[
                Tlv::new(0x01, &session),
                Tlv::new(0x02, &file),
                Tlv::new(0x03, &info),
            ],
        )?;
        validate_response(&response, SERVICE_UIM, UIM_READ_RECORD)?;
        Ok(read_result(&response, 0x11)?.unwrap_or_default())
    }

    fn read_transparent_fallback(&self, file_id: u16) -> Result<Vec<u8>, UimClientError> {
        let mut last_error = None;
        for path in SIM_PATHS {
            match self.read_transparent(file_id, path) {
                Ok(data) => return Ok(data),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| malformed("transparent SIM file")))
    }

    fn read_transparent(&self, file_id: u16, path: &[u8]) -> Result<Vec<u8>, UimClientError> {
        let session = session_value(SESSION_PRIMARY_GW_PROVISIONING);
        let file = file_value(file_id, path)?;
        let read_info = [0_u8; 4];
        let response = self.uim.request(
            UIM_READ_TRANSPARENT,
            &[
                Tlv::new(0x01, &session),
                Tlv::new(0x02, &file),
                Tlv::new(0x03, &read_info),
            ],
        )?;
        validate_response(&response, SERVICE_UIM, UIM_READ_TRANSPARENT)?;
        if let Some(data) = read_result(&response, 0x11)? {
            return Ok(data);
        }
        // Keep the Go QMI parser's compatibility fallback for older modem
        // firmware that places transparent data directly in TLV 0x10.
        Ok(response.find_tlv(0x10).unwrap_or_default().to_vec())
    }
}

impl NativeMccMnc {
    fn from_plmn(plmn: &str) -> Option<Self> {
        let digits = plmn.as_bytes();
        if !(digits.len() == 5 || digits.len() == 6) || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let mut mcc = [0; 3];
        mcc.copy_from_slice(&digits[..3]);
        let mut mnc = [0; 3];
        mnc[..digits.len() - 3].copy_from_slice(&digits[3..]);
        Some(Self {
            mcc,
            mnc,
            mnc_len: (digits.len() - 3) as u8,
        })
    }
}

#[derive(Clone, Debug)]
struct RecordLayout {
    path: Vec<u8>,
    record_size: u16,
    record_count: u16,
    first_record: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug)]
struct FileAttributes {
    record_size: u16,
    record_count: u16,
}

fn parse_file_attributes(response: &OwnedPacket) -> Result<FileAttributes, UimClientError> {
    let value = response
        .find_tlv(0x11)
        .ok_or_else(|| malformed("file attributes TLV"))?;
    let fixed = value
        .get(..26)
        .ok_or_else(|| malformed("file attributes TLV length"))?;
    let raw_len = usize::from(u16::from_le_bytes([fixed[24], fixed[25]]));
    if value.len() < 26 + raw_len {
        return Err(malformed("file attributes raw data length"));
    }
    Ok(FileAttributes {
        record_size: u16::from_le_bytes([fixed[5], fixed[6]]),
        record_count: u16::from_le_bytes([fixed[7], fixed[8]]),
    })
}

fn read_result(response: &OwnedPacket, kind: u8) -> Result<Option<Vec<u8>>, UimClientError> {
    let Some(value) = response.find_tlv(kind) else {
        return Ok(None);
    };
    let length = value
        .get(..2)
        .map(|bytes| usize::from(u16::from_le_bytes([bytes[0], bytes[1]])))
        .ok_or_else(|| malformed("UIM read result length"))?;
    let end = 2_usize
        .checked_add(length)
        .ok_or_else(|| malformed("UIM read result length"))?;
    let data = value
        .get(2..end)
        .ok_or_else(|| malformed("UIM read result data"))?;
    Ok(Some(data.to_vec()))
}

fn session_value(session_type: u8) -> [u8; 2] {
    [session_type, 0]
}

fn file_value(file_id: u16, path: &[u8]) -> Result<Vec<u8>, UimClientError> {
    let path_len = u8::try_from(path.len()).map_err(|_| malformed("UIM file path length"))?;
    let mut value = Vec::with_capacity(3 + path.len());
    value.extend_from_slice(&file_id.to_le_bytes());
    value.push(path_len);
    value.extend_from_slice(path);
    Ok(value)
}

fn decode_opl_plmn(data: &[u8]) -> Option<String> {
    let [first, second, third] = data.get(..3)? else {
        return None;
    };
    let nibbles = [
        first & 0x0f,
        first >> 4,
        second & 0x0f,
        third & 0x0f,
        third >> 4,
        second >> 4,
    ];
    let mut plmn = String::with_capacity(6);
    for nibble in nibbles {
        match nibble {
            0..=9 => plmn.push(char::from(b'0' + nibble)),
            0x0f => plmn.push('x'),
            _ => return None,
        }
    }
    while plmn.ends_with('x') {
        plmn.pop();
    }
    Some(plmn)
}

fn decode_swapped_bcd(data: &[u8]) -> String {
    let mut digits = String::with_capacity(data.len() * 2);
    for byte in data.iter().copied() {
        for nibble in [byte & 0x0f, byte >> 4] {
            if nibble <= 9 {
                digits.push(char::from(b'0' + nibble));
            }
        }
    }
    digits
}

fn parse_imsi_file(data: &[u8]) -> Result<String, UimClientError> {
    if data.len() <= 1 {
        return Err(malformed("IMSI length"));
    }
    let encoded_len = usize::from(data[0]);
    let body = &data[1..];
    let bcd = if encoded_len <= body.len() {
        &body[..encoded_len]
    } else {
        body
    };
    let mut imsi = decode_swapped_bcd(bcd);
    // EF-IMSI's first decoded nibble is the identity type/parity marker,
    // matching the established Go QMI parser.
    if !imsi.is_empty() {
        imsi.remove(0);
    }
    if imsi.is_empty() || !imsi.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(malformed("empty or invalid IMSI"));
    }
    Ok(imsi)
}

fn native_mcc_mnc_from_imsi(imsi: &str, ef_ad: &[u8]) -> Result<NativeMccMnc, UimClientError> {
    let mnc_len = if ef_ad.len() >= 4 && matches!(ef_ad[3], 2 | 3) {
        usize::from(ef_ad[3])
    } else {
        2
    };
    let digits = imsi.as_bytes();
    if digits.len() < 3 + mnc_len || !digits.iter().all(u8::is_ascii_digit) {
        return Err(malformed("IMSI/MNC length"));
    }
    let mut mcc = [0; 3];
    mcc.copy_from_slice(&digits[..3]);
    let mut mnc = [0; 3];
    mnc[..mnc_len].copy_from_slice(&digits[3..3 + mnc_len]);
    Ok(NativeMccMnc {
        mcc,
        mnc,
        mnc_len: mnc_len as u8,
    })
}

fn trim_file_padding(data: &[u8]) -> &[u8] {
    let mut end = data.len();
    while end > 0 && matches!(data[end - 1], 0x00 | 0xff) {
        end -= 1;
    }
    &data[..end]
}

fn malformed(field: &'static str) -> UimClientError {
    crate::CodecError::MalformedField(field).into()
}

#[cfg(test)]
mod tests {
    use super::{
        NativeMccMnc, decode_opl_plmn, decode_swapped_bcd, native_mcc_mnc_from_imsi,
        parse_imsi_file, trim_file_padding,
    };

    #[test]
    fn opl_decodes_swapped_plmn_and_trailing_wildcard_like_go() {
        assert_eq!(decode_opl_plmn(&[0x13, 0x00, 0x62]), Some("310260".into()));
        assert_eq!(decode_opl_plmn(&[0x13, 0xf0, 0x62]), Some("31026".into()));
        assert_eq!(decode_opl_plmn(&[0x13, 0x0a, 0x62]), None);
    }

    #[test]
    fn home_plmn_accepts_only_exact_five_or_six_digit_values() {
        let two_digit = NativeMccMnc::from_plmn("31026").unwrap();
        assert_eq!((two_digit.mcc(), two_digit.mnc()), ("310", "26"));
        let three_digit = NativeMccMnc::from_plmn("310260").unwrap();
        assert_eq!((three_digit.mcc(), three_digit.mnc()), ("310", "260"));
        assert!(NativeMccMnc::from_plmn("31x260").is_none());
        assert!(NativeMccMnc::from_plmn("3102").is_none());
    }

    #[test]
    fn swapped_bcd_and_padding_match_go_qmi_helpers() {
        assert_eq!(decode_swapped_bcd(&[0x21, 0x43, 0xf5]), "12345");
        assert_eq!(trim_file_padding(&[1, 2, 0xff, 0x00]), &[1, 2]);
        assert!(trim_file_padding(&[0xff, 0x00]).is_empty());
    }

    #[test]
    fn imsi_and_ef_ad_fallback_keeps_two_and_three_digit_mnc() {
        let imsi = parse_imsi_file(&[15, 0x31, 0x01, 0x62, 0x10, 0x32, 0x54, 0x76, 0x98]).unwrap();
        assert_eq!(imsi, "310260123456789");
        let three_digit = native_mcc_mnc_from_imsi(&imsi, &[0, 0, 0, 3]).unwrap();
        assert_eq!((three_digit.mcc(), three_digit.mnc()), ("310", "260"));
        let two_digit = native_mcc_mnc_from_imsi(&imsi, &[0, 0, 0, 2]).unwrap();
        assert_eq!((two_digit.mcc(), two_digit.mnc()), ("310", "26"));
        let default_two_digit = native_mcc_mnc_from_imsi(&imsi, &[]).unwrap();
        assert_eq!(
            (default_two_digit.mcc(), default_two_digit.mnc()),
            ("310", "26")
        );
    }
}
