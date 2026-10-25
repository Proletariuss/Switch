use serde::Serialize;
use std::fmt;

use proletarius_sim::{
    ApduTransport, ISIM_AID_PREFIX, decode_msisdn_record, decode_smsp_record, exchange_apdu,
    select_application_apdu, select_file_apdu,
};

use crate::IdentityError;

const MAX_METADATA_FILE_BYTES: usize = 32_767;
const MAX_METADATA_RECORDS: usize = 255;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ReaderSimMetadata {
    pub iccid: String,
    pub imsi: String,
    pub mcc: String,
    pub mnc: String,
    pub application_aid: Vec<u8>,
    pub gid1: String,
    pub gid2: String,
    pub spn: String,
    pub phone_number: String,
    pub operator_name: String,
    pub operator_source: String,
    pub smsc: String,
    pub smsc_psi: String,
    pub smsc_target: String,
    pub private_identity: String,
    pub public_identity: String,
    pub home_domain: String,
    pub isim_aid: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct FileAttributes {
    structure: u8,
    record_size: usize,
    record_count: usize,
    file_size: usize,
}

pub fn read_sim_metadata_auto_with_transport<T: ApduTransport>(
    transport: &mut T,
) -> Result<ReaderSimMetadata, IdentityError>
where
    T::Error: fmt::Display,
{
    read_sim_metadata_with_transport(transport, &[])
}

/// Read the complete SIM metadata snapshot for a module-backed SIM interface.
/// This intentionally uses the module EF-DIR tolerance that skips exact
/// `6986` READ RECORD responses while requiring an accessible USIM AID.
pub fn read_sim_metadata_module_with_transport<T: ApduTransport>(
    transport: &mut T,
) -> Result<ReaderSimMetadata, IdentityError>
where
    T::Error: fmt::Display,
{
    let aid = crate::discover_module_usim_aid_with_transport(transport)?;
    read_sim_metadata_with_policy(transport, &aid, true)
}

pub fn read_sim_metadata_with_transport<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
) -> Result<ReaderSimMetadata, IdentityError>
where
    T::Error: fmt::Display,
{
    read_sim_metadata_with_policy(transport, aid, false)
}

fn read_sim_metadata_with_policy<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    module_dir_status_quirk: bool,
) -> Result<ReaderSimMetadata, IdentityError>
where
    T::Error: fmt::Display,
{
    let carrier = if aid.is_empty() {
        crate::read_carrier_identity_auto_with_transport(transport)?
    } else {
        crate::read_carrier_identity_with_transport(transport, aid)?
    };
    let aid = carrier.application_aid.clone();
    let mut metadata = ReaderSimMetadata {
        iccid: carrier.identity.iccid.clone(),
        imsi: carrier.identity.imsi.clone(),
        mcc: carrier.identity.mcc.clone(),
        mnc: carrier.identity.mnc.clone(),
        application_aid: carrier.application_aid.clone(),
        gid1: carrier.gid1.clone(),
        gid2: carrier.gid2.clone(),
        spn: carrier.spn.clone(),
        operator_name: carrier.spn.clone(),
        operator_source: if carrier.spn.is_empty() {
            String::new()
        } else {
            "ef_spn".into()
        },
        ..ReaderSimMetadata::default()
    };

    if let Ok(records) = read_records(transport, &aid, &[0x6f, 0x40]) {
        metadata.phone_number = records
            .iter()
            .find_map(|record| decode_msisdn_record(record).ok().flatten())
            .unwrap_or_default();
    }
    if metadata.operator_name.is_empty() {
        if let Ok(records) = read_records(transport, &aid, &[0x6f, 0xc5]) {
            for record in &records {
                if let Some(name) = decode_pnn_record(record) {
                    metadata.operator_name = name;
                    metadata.operator_source = "ef_pnn".into();
                    break;
                }
            }
        }
    }
    if let Ok(records) = read_records(transport, &aid, &[0x6f, 0x42]) {
        metadata.smsc = records
            .iter()
            .find_map(|record| decode_smsp_record(record).ok().flatten())
            .unwrap_or_default();
    }
    if let Ok(records) = read_path_records(transport, &aid, &[0x7f, 0x10, 0x6f, 0xe5]) {
        metadata.smsc_psi = first_text(&records).unwrap_or_default();
    }

    let isim_aid_result = if module_dir_status_quirk {
        crate::discover_module_aid_with_transport(transport, ISIM_AID_PREFIX)
    } else {
        crate::discover_aid_with_transport(transport, ISIM_AID_PREFIX)
    };
    if let Ok(isim_aid) = isim_aid_result {
        if let Ok((private_identity, public_identity, home_domain, isim_psi)) =
            read_isim(transport, &isim_aid)
        {
            metadata.private_identity = private_identity;
            metadata.public_identity = public_identity;
            metadata.home_domain = if home_domain.is_empty() {
                metadata
                    .private_identity
                    .split_once('@')
                    .map(|(_, domain)| domain.to_owned())
                    .unwrap_or_default()
            } else {
                home_domain
            };
            if !isim_psi.is_empty() {
                metadata.smsc_psi = isim_psi;
            }
            metadata.isim_aid = isim_aid;
        }
    }
    metadata.smsc_target = if !metadata.smsc_psi.trim().is_empty() {
        metadata.smsc_psi.trim().to_owned()
    } else if !metadata.smsc.trim().is_empty() {
        format!("tel:{}", metadata.smsc.trim())
    } else {
        String::new()
    };
    Ok(metadata)
}

fn read_isim<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
) -> Result<(String, String, String, String), String>
where
    T::Error: fmt::Display,
{
    let private_identity = read_application_text(transport, aid, &[0x6f, 0x02])?;
    let home_domain = read_application_text(transport, aid, &[0x6f, 0x03]).unwrap_or_default();
    let public_identity = read_application_text_records(transport, aid, &[0x6f, 0x04])?;
    let psi = read_path_records(transport, aid, &[0x7f, 0x10, 0x6f, 0xe5])
        .ok()
        .and_then(|records| first_text(&records))
        .unwrap_or_default();
    Ok((private_identity, public_identity, home_domain, psi))
}

fn read_application_text<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    file: &[u8],
) -> Result<String, String>
where
    T::Error: fmt::Display,
{
    let data = read_transparent(transport, aid, file)?;
    decode_text(&data).ok_or_else(|| "card text file is empty or malformed".into())
}

fn read_application_text_records<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    file: &[u8],
) -> Result<String, String>
where
    T::Error: fmt::Display,
{
    let records = read_records(transport, aid, file)?;
    first_text(&records).ok_or_else(|| "card text records contain no value".into())
}

fn read_transparent<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    file: &[u8],
) -> Result<Vec<u8>, String>
where
    T::Error: fmt::Display,
{
    let attributes = select_file(transport, aid, file, false)?;
    if attributes.structure != 0x41
        || attributes.file_size == 0
        || attributes.file_size > MAX_METADATA_FILE_BYTES
    {
        return Err("unexpected transparent file attributes".into());
    }
    let mut output = Vec::with_capacity(attributes.file_size);
    let mut offset = 0usize;
    while offset < attributes.file_size {
        let length = (attributes.file_size - offset).min(255);
        if offset > u16::MAX as usize {
            return Err("transparent file offset exceeds short APDU limit".into());
        }
        let command = proletarius_sim::read_binary_apdu(offset as u16, length as u8);
        let response = execute(transport, &command)?;
        let data = response.success_data().map_err(|error| error.to_string())?;
        if data.len() != length {
            return Err("transparent file read returned an unexpected length".into());
        }
        output.extend_from_slice(data);
        offset += length;
    }
    Ok(output)
}

fn read_records<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    file: &[u8],
) -> Result<Vec<Vec<u8>>, String>
where
    T::Error: fmt::Display,
{
    read_path_records(transport, aid, file)
}

fn read_path_records<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    path: &[u8],
) -> Result<Vec<Vec<u8>>, String>
where
    T::Error: fmt::Display,
{
    let attributes = select_file(transport, aid, path, path.len() > 2)?;
    if attributes.structure != 0x42
        || attributes.record_size == 0
        || attributes.record_size > 255
        || attributes.record_count == 0
        || attributes.record_count > MAX_METADATA_RECORDS
    {
        return Err("unexpected linear-fixed file attributes".into());
    }
    let mut records = Vec::with_capacity(attributes.record_count);
    for number in 1..=attributes.record_count {
        let command = [0, 0xb2, number as u8, 0x04, attributes.record_size as u8];
        let response = execute(transport, &command)?;
        let data = response.success_data().map_err(|error| error.to_string())?;
        if data.len() != attributes.record_size {
            return Err("linear-fixed record returned an unexpected length".into());
        }
        records.push(data.to_vec());
    }
    Ok(records)
}

fn select_file<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    path: &[u8],
    is_path: bool,
) -> Result<FileAttributes, String>
where
    T::Error: fmt::Display,
{
    let select_application = select_application_apdu(aid).map_err(|error| error.to_string())?;
    execute(transport, &select_application)?
        .success_data()
        .map_err(|error| error.to_string())?;
    let select = if is_path {
        if path.is_empty() || path.len() % 2 != 0 || path.len() > u8::MAX as usize {
            return Err("invalid card file path".into());
        }
        let mut command = vec![0x00, 0xa4, 0x08, 0x04, path.len() as u8];
        command.extend_from_slice(path);
        command
    } else {
        let file_id: [u8; 2] = path
            .try_into()
            .map_err(|_| "invalid elementary file identifier")?;
        select_file_apdu(file_id)
    };
    let response = execute(transport, &select)?;
    let fcp = response.success_data().map_err(|error| error.to_string())?;
    parse_file_attributes(fcp)
}

fn execute<T: ApduTransport>(
    transport: &mut T,
    command: &[u8],
) -> Result<proletarius_sim::ApduResponse, String>
where
    T::Error: fmt::Display,
{
    exchange_apdu(transport, command).map_err(|error| error.to_string())
}

fn parse_file_attributes(fcp: &[u8]) -> Result<FileAttributes, String> {
    let (tag, body) = parse_tlv(fcp)?.ok_or_else(|| "empty FCP template".to_owned())?;
    if tag != 0x62 {
        return Err("card response is not an FCP template".into());
    }
    let mut attributes = FileAttributes::default();
    let mut has_descriptor = false;
    let mut rest = body;
    while let Some((tag, value, tail)) = parse_tlv_part(rest)? {
        match tag {
            0x82 if matches!(value.len(), 2 | 5) => {
                attributes.structure = value[0];
                has_descriptor = true;
                if value.len() == 5 {
                    attributes.record_size = usize::from(u16::from_be_bytes([value[2], value[3]]));
                    attributes.record_count = usize::from(value[4]);
                }
            }
            0x80 if value.len() == 2 => {
                attributes.file_size = usize::from(u16::from_be_bytes([value[0], value[1]]));
            }
            _ => {}
        }
        rest = tail;
    }
    if !has_descriptor {
        return Err("FCP is missing a file descriptor".into());
    }
    Ok(attributes)
}

fn parse_tlv(data: &[u8]) -> Result<Option<(u8, &[u8])>, String> {
    let Some((&tag, rest)) = data.split_first() else {
        return Ok(None);
    };
    let (length, value) = parse_length(rest)?;
    if value.len() < length {
        return Err("truncated FCP TLV".into());
    }
    Ok(Some((tag, &value[..length])))
}

fn parse_tlv_part(data: &[u8]) -> Result<Option<(u8, &[u8], &[u8])>, String> {
    let Some((&tag, rest)) = data.split_first() else {
        return Ok(None);
    };
    let (length, value) = parse_length(rest)?;
    if value.len() < length {
        return Err("truncated FCP child TLV".into());
    }
    Ok(Some((tag, &value[..length], &value[length..])))
}

fn parse_length(data: &[u8]) -> Result<(usize, &[u8]), String> {
    let (&first, rest) = data.split_first().ok_or("missing TLV length")?;
    match first {
        0x00..=0x7f => Ok((usize::from(first), rest)),
        0x81 => {
            let (&length, tail) = rest.split_first().ok_or("truncated TLV length")?;
            Ok((usize::from(length), tail))
        }
        0x82 => {
            let pair = rest.get(..2).ok_or("truncated TLV length")?;
            Ok((
                usize::from(u16::from_be_bytes([pair[0], pair[1]])),
                &rest[2..],
            ))
        }
        _ => Err("unsupported TLV length encoding".into()),
    }
}

fn first_text(records: &[Vec<u8>]) -> Option<String> {
    records.iter().find_map(|record| decode_text(record))
}

fn decode_text(bytes: &[u8]) -> Option<String> {
    let trimmed = trim_padding(bytes);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed[0] == 0x80 {
        let length = usize::from(*trimmed.get(1)?);
        let value = trimmed.get(2..2usize.checked_add(length)?)?;
        let text = String::from_utf8(value.to_vec()).ok()?;
        return (!text.is_empty()).then_some(text);
    }
    if trimmed.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
        return Some(String::from_utf8(trimmed.to_vec()).ok()?);
    }
    None
}

fn decode_pnn_record(record: &[u8]) -> Option<String> {
    let mut rest = trim_padding(record);
    let mut full = String::new();
    let mut short = String::new();
    while !rest.is_empty() {
        let tag = *rest.first()?;
        let (length, value) = parse_length(&rest[1..]).ok()?;
        let header = 1 + length_field_size(&rest[1..])?;
        let value = value.get(..length)?;
        let target = match tag {
            0x43 => &mut full,
            0x45 => &mut short,
            _ => break,
        };
        if target.is_empty() {
            *target = decode_network_name(value).unwrap_or_default();
        }
        rest = rest.get(header + length..)?;
    }
    if !full.is_empty() {
        Some(full)
    } else if !short.is_empty() {
        Some(short)
    } else {
        None
    }
}

fn length_field_size(data: &[u8]) -> Option<usize> {
    let first = *data.first()?;
    match first {
        0x00..=0x7f => Some(1),
        0x81 => Some(2),
        0x82 => Some(3),
        _ => None,
    }
}

fn decode_network_name(value: &[u8]) -> Option<String> {
    let (&header, payload) = value.split_first()?;
    let decoded = match (header >> 4) & 0x07 {
        0 => {
            let spare_bits = usize::from(header & 0x07);
            let total_bits = payload.len().checked_mul(8)?;
            let septet_count = total_bits.checked_sub(spare_bits)? / 7;
            let septets = proletarius_sms::septets::unpack(payload, septet_count, 0).ok()?;
            proletarius_sms::alphabet::decode(&septets).ok()?
        }
        1 => crate::decode_ucs2(payload)?,
        _ => decode_alpha(payload)?,
    };
    let text = decoded.replace('\0', "").trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn decode_alpha(data: &[u8]) -> Option<String> {
    let data = trim_padding(data);
    if data.is_empty() {
        return None;
    }
    let decoded = match data[0] {
        0x80 => crate::decode_ucs2(&data[1..])?,
        0x81 => crate::decode_compressed_ucs2(data, 1)?,
        0x82 => crate::decode_compressed_ucs2(data, 2)?,
        _ if data.iter().all(|byte| (0x20..=0x7e).contains(byte)) => {
            String::from_utf8(data.to_vec()).ok()?
        }
        _ => proletarius_sms::alphabet::decode(data).ok()?,
    };
    let value = decoded.replace('\0', "").trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn trim_padding(data: &[u8]) -> &[u8] {
    let mut end = data.len();
    while end > 0 && matches!(data[end - 1], 0x00 | 0xff) {
        end -= 1;
    }
    &data[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_go_fcp_descriptor_and_file_size() {
        let fcp = [
            0x62, 0x0b, 0x82, 0x05, 0x42, 0x21, 0x00, 0x14, 0x02, 0x80, 0x02, 0x00, 0x28,
        ];
        assert_eq!(
            parse_file_attributes(&fcp).unwrap(),
            FileAttributes {
                structure: 0x42,
                record_size: 20,
                record_count: 2,
                file_size: 40,
            }
        );
    }

    #[test]
    fn text_psi_and_pnn_decoders_match_go_wire_encodings() {
        assert_eq!(
            decode_text(&[0x80, 0x03, b's', b'i', b'p']),
            Some("sip".into())
        );
        assert_eq!(
            decode_text(b"sip:card@example.test\xff"),
            Some("sip:card@example.test".into())
        );
        assert_eq!(
            decode_pnn_record(&[0x43, 0x02, 0x01, 0x4f, 0x45, 0x02, 0x01, 0x58]),
            Some("O".into())
        );
        let o2 = proletarius_sms::septets::pack(b"O2", 0).unwrap();
        assert_eq!(
            decode_network_name(&[0x02, o2[0], o2[1]]),
            Some("O2".into())
        );
        let bal = proletarius_sms::septets::pack(b"BAL", 0).unwrap();
        assert_eq!(
            decode_network_name(&[0x03, bal[0], bal[1], bal[2]]),
            Some("BAL".into())
        );
        assert_eq!(
            decode_network_name(&[0x10, 0x00, 0x48, 0x00, 0x69]),
            Some("Hi".into())
        );
    }

    #[test]
    fn malformed_optional_text_and_fcp_fail_closed() {
        assert!(parse_file_attributes(&[0x62, 0x02, 0x82, 0x00]).is_err());
        assert_eq!(decode_text(&[0x80, 0x05, b'x']), None);
        assert_eq!(decode_pnn_record(&[0x43, 0x7f, 0x10]), None);
    }
}
