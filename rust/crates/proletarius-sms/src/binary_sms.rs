//! Conservative classification and safe text projection of binary SMS.
//! This mirrors the currently used Go USB-AT compatibility classifier without
//! attempting to decrypt SIM OTA or treat arbitrary bytes as UTF-8.

use crate::{deliver::Message, oma_cp, user_data::Content};

const WAP_PUSH_PORT: u16 = 2948;

#[derive(Default)]
struct Ports {
    port16: Option<u16>,
    port8: Option<u8>,
}

impl Ports {
    fn parse(message: &Message) -> Self {
        let mut result = Self::default();
        for (id, value) in &message.udh_elements {
            match *id {
                0x05 if value.len() >= 4 => {
                    result.port16 = Some(u16::from_be_bytes([value[0], value[1]]));
                }
                0x04 if value.len() >= 2 => result.port8 = Some(value[0]),
                _ => {}
            }
        }
        result
    }

    fn preferred(&self) -> Option<u16> {
        self.port16.or_else(|| self.port8.map(u16::from))
    }
}

struct Wsp<'a> {
    tid: u8,
    pdu: u8,
    content_type: String,
    body: &'a [u8],
}

fn parse_wsp(data: &[u8]) -> Option<Wsp<'_>> {
    if data.len() < 4 || !matches!(data[1], 0x06 | 0x07) {
        return None;
    }
    let header_length = usize::from(data[2]);
    let body_start = 3usize.checked_add(header_length)?;
    let headers = data.get(3..body_start)?;
    Some(Wsp {
        tid: data[0],
        pdu: data[1],
        content_type: parse_content_type(headers),
        body: data.get(body_start..)?,
    })
}

fn parse_content_type(headers: &[u8]) -> String {
    let Some(first) = headers.first().copied() else {
        return String::new();
    };
    if first & 0x80 != 0 {
        return content_type_token(first & 0x7f);
    }
    if (0x20..=0x7e).contains(&first) {
        return normalize_content_type(read_cstring(headers));
    }
    let length = usize::from(first);
    let Some(value) = headers.get(1..1usize.saturating_add(length)) else {
        return String::new();
    };
    if let Some(token) = value.first().copied().filter(|byte| byte & 0x80 != 0) {
        content_type_token(token & 0x7f)
    } else {
        normalize_content_type(read_cstring(value))
    }
}

fn content_type_token(token: u8) -> String {
    match token {
        0x2e => "application/vnd.wap.sic".into(),
        0x30 => "application/vnd.wap.connectivity-wbxml".into(),
        0x31 => "application/vnd.wap.slc".into(),
        0x3e => "application/vnd.wap.mms-message".into(),
        other => format!("token:0x{other:02x}"),
    }
}

fn read_cstring(data: &[u8]) -> &str {
    let end = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(data.len());
    std::str::from_utf8(&data[..end]).unwrap_or("")
}

fn normalize_content_type(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn is_oma_type(value: &str) -> bool {
    value == "application/vnd.wap.connectivity-wbxml"
}

fn is_si_type(value: &str) -> bool {
    matches!(value, "application/vnd.wap.sic" | "application/vnd.wap.si")
}

fn is_sl_type(value: &str) -> bool {
    matches!(value, "application/vnd.wap.slc" | "application/vnd.wap.sl")
}

fn is_mms_type(value: &str) -> bool {
    value == "application/vnd.wap.mms-message"
}

fn wbxml_public_id(data: &[u8]) -> Option<u32> {
    if data.len() < 3 || !(1..=3).contains(&data[0]) {
        return None;
    }
    read_mb_uint32(data, 1).map(|(value, _)| value)
}

fn read_mb_uint32(data: &[u8], start: usize) -> Option<(u32, usize)> {
    let mut value = 0u32;
    for offset in 0..5 {
        let byte = *data.get(start.checked_add(offset)?)?;
        value = value.wrapping_shl(7) | u32::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Some((value, offset + 1));
        }
    }
    None
}

fn extract_url(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    for prefix in ["https://", "http://"] {
        let Some(start) = text.find(prefix) else {
            continue;
        };
        let tail = &text[start..];
        let end = tail
            .bytes()
            .position(|byte| {
                byte == 0
                    || byte.is_ascii_whitespace()
                    || matches!(byte, b'"' | b'\'' | b'<' | b'>')
            })
            .unwrap_or(tail.len());
        return Some(tail[..end].to_string());
    }
    None
}

fn printable_hint(data: &[u8], max: usize) -> String {
    let output: String = data
        .iter()
        .copied()
        .filter(|byte| (0x20..=0x7e).contains(byte) || matches!(byte, b'\n' | b'\r' | b'\t'))
        .take(max)
        .map(char::from)
        .collect();
    output.trim().to_string()
}

fn contains_mms_notification(data: &[u8]) -> bool {
    data.windows(2).any(|window| window == [0x8c, 0x82])
}

fn tagged_cstring(data: &[u8], tag: u8) -> Option<String> {
    data.windows(2)
        .enumerate()
        .find(|(_, window)| window[0] == tag)
        .and_then(|(index, _)| {
            let value = read_cstring(data.get(index + 1..)?);
            (!value.is_empty()).then(|| value.to_string())
        })
}

fn tagged_uint(data: &[u8], tag: u8) -> Option<u32> {
    data.iter()
        .position(|byte| *byte == tag)
        .and_then(|index| read_mb_uint32(data, index + 1).map(|(value, _)| value))
}

fn sim_ota_header(data: &[u8]) -> Option<(u16, u8, u8)> {
    if data.len() < 6 {
        return None;
    }
    let header_length = usize::from(data[1]);
    if header_length < 5 || 2usize.checked_add(header_length)? > data.len() {
        return None;
    }
    Some((u16::from_be_bytes([data[2], data[3]]), data[4], data[5]))
}

/// Return the compatibility text shown for a binary SMS.
pub fn format(message: &Message) -> Option<String> {
    let Content::Binary(raw_payload) = &message.content else {
        return None;
    };
    let ports = Ports::parse(message);
    let destination = ports.preferred();
    let wsp = parse_wsp(raw_payload);
    let (content_type, payload) = wsp
        .as_ref()
        .map(|value| (value.content_type.as_str(), value.body))
        .unwrap_or(("", raw_payload));

    let mut lines = Vec::new();
    let (label, encrypted) = if destination == Some(WAP_PUSH_PORT) || is_oma_type(content_type) {
        lines.extend(wsp.as_ref().and_then(|value| {
            (!value.content_type.is_empty()).then(|| format!("content_type={}", value.content_type))
        }));
        if let Some(value) = &wsp {
            lines.push(format!(
                "wsp_tid=0x{:02x} pdu=0x{:02x}",
                value.tid, value.pdu
            ));
        }
        match oma_cp::decode(payload) {
            Ok(summary) => lines.extend(summary.trim().lines().map(str::to_string)),
            Err(()) => {
                lines.push("wbxml_decode=failed (可能加密/非明文)".into());
                lines.push("security=可能加密".into());
            }
        }
        ("OMA CP 运营商配置短信", false)
    } else if is_si_type(content_type) || matches!(wbxml_public_id(payload), Some(5 | 6)) {
        if let Some(url) = extract_url(payload) {
            lines.push(format!("url={url}"));
        }
        let hint = printable_hint(payload, 80);
        if !hint.is_empty() {
            lines.push(format!("hint={hint}"));
        }
        ("WAP SI Push", false)
    } else if is_sl_type(content_type) || wbxml_public_id(payload) == Some(7) {
        if let Some(url) = extract_url(payload) {
            lines.push(format!("url={url}"));
        }
        let hint = printable_hint(payload, 80);
        if !hint.is_empty() {
            lines.push(format!("hint={hint}"));
        }
        ("WAP SL Push", false)
    } else if is_mms_type(content_type) || contains_mms_notification(payload) {
        if let Some(value) = tagged_cstring(payload, 0x98) {
            lines.push(format!("x_mms_transaction_id={value}"));
        }
        if let Some(value) = tagged_cstring(payload, 0x83) {
            lines.push(format!("content_location={value}"));
        }
        if let Some(value) = tagged_uint(payload, 0x8e) {
            lines.push(format!("message_size={value}"));
        }
        if let Some(value) = extract_url(payload) {
            lines.push(format!("url={value}"));
        }
        ("MMS Notification", false)
    } else if !is_oma_type(content_type)
        && !is_si_type(content_type)
        && !is_sl_type(content_type)
        && !is_mms_type(content_type)
        && destination != Some(WAP_PUSH_PORT)
        && (message.protocol_id == 0x7f
            || (message.dcs & 0x10 != 0
                && message.dcs & 0x03 == 0x02
                && sim_ota_header(payload).is_some()))
    {
        if let Some((spi, kic, kid)) = sim_ota_header(payload) {
            lines.push(format!("spi=0x{spi:04x} kic=0x{kic:02x} kid=0x{kid:02x}"));
        }
        lines.push("decrypt=not_attempted".into());
        lines.push("security=可能加密".into());
        ("SIM OTA 23.048", true)
    } else {
        if let Some(value) = destination {
            lines.push(format!("dest_port={value}"));
        }
        ("二进制数据", false)
    };

    let mut result = format!("[{label}]\n");
    if !lines.iter().any(|line| line.starts_with("dest_port="))
        && let Some(value) = destination
    {
        result.push_str(&format!("dest_port={value}\n"));
    }
    for line in lines {
        result.push_str(line.trim());
        result.push('\n');
    }
    if encrypted && !result.contains("security=") {
        result.push_str("security=可能加密\n");
    }
    result.push_str("raw=");
    for byte in raw_payload {
        result.push_str(&format!("{byte:02x}"));
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(protocol_id: u8, dcs: u8, payload: Vec<u8>) -> Message {
        Message {
            sender: "65075".into(),
            timestamp: [0; 7],
            service_center_time: None,
            protocol_id,
            dcs,
            concatenation: None,
            udh_elements: Vec::new(),
            content: Content::Binary(payload),
        }
    }

    fn wsp(content_type: &[u8], body: &[u8]) -> Vec<u8> {
        let mut headers = Vec::new();
        if content_type.first().is_some_and(|byte| byte & 0x80 != 0) {
            headers.extend_from_slice(content_type);
        } else {
            headers.extend_from_slice(content_type);
            headers.push(0);
        }
        let mut output = vec![1, 6, headers.len() as u8];
        output.extend(headers);
        output.extend(body);
        output
    }

    fn oma_wbxml() -> Vec<u8> {
        let mut data = vec![0x03, 0x0b, 0x6a, 0x00, 0x45, 0xc6, 0xa4, 0x01];
        data.extend_from_slice(&[0x87, 0x11, 0x06, 0x03]);
        data.extend_from_slice(b"internet\0");
        data.extend_from_slice(&[0x01, 0x87, 0x08, 0x06, 0x03]);
        data.extend_from_slice(b"internet.telekom\0");
        data.extend_from_slice(&[0x01, 0x87, 0x10, 0x06, 0x73, 0x01]);
        data.extend_from_slice(&[0x87, 0x09, 0x06, 0x49, 0x01, 0x01]);
        data.extend_from_slice(&[0xc6, 0x00, 0x01, 0xa4, 0x01]);
        data.extend_from_slice(&[0x87, 0x1c, 0x06, 0x91, 0x01]);
        data.extend_from_slice(&[0x87, 0x07, 0x06, 0x03]);
        data.extend_from_slice(b"Telekom\0");
        data.extend_from_slice(&[0x01, 0x87, 0x11, 0x06, 0x03]);
        data.extend_from_slice(b"internet\0");
        data.extend_from_slice(&[0x01, 0x01, 0x01]);
        data
    }

    #[test]
    fn historical_go_encrypted_oma_vector_keeps_security_diagnostic() {
        let payload = include_bytes!("../testdata/oma-cp/encrypted.bin").to_vec();
        let mut encrypted = message(0, 4, payload);
        encrypted.udh_elements = vec![(0x05, vec![0x0b, 0x84, 0x23, 0xf0])];
        let formatted = format(&encrypted).unwrap();
        assert!(formatted.contains("[OMA CP 运营商配置短信]"));
        assert!(formatted.contains("wbxml_decode=failed (可能加密/非明文)"));
        assert!(formatted.contains("security=可能加密"));
    }

    #[test]
    fn matches_go_classifier_for_port_based_oma_and_unknown() {
        let mut oma = message(0, 4, vec![0x03, 0x0b, 0x6a, 0x00]);
        oma.udh_elements = vec![
            (0x04, vec![0x23, 0xf0]),
            (0x05, vec![0x0b, 0x84, 0x23, 0xf0]),
        ];
        let formatted = format(&oma).unwrap();
        assert!(formatted.contains("[OMA CP 运营商配置短信]"));
        assert!(formatted.contains("dest_port=2948"));
        assert!(formatted.contains("raw=030b6a00"));

        let unknown = format(&message(0, 4, vec![0xde, 0xad, 0xbe, 0xef])).unwrap();
        assert_eq!(unknown, "[二进制数据]\nraw=deadbeef");
    }

    #[test]
    fn unwraps_wsp_token_and_formats_oma_configuration() {
        let payload = wsp(&[0xb0], &oma_wbxml());
        let formatted = format(&message(0, 4, payload)).unwrap();
        for expected in [
            "[OMA CP 运营商配置短信]",
            "content_type=application/vnd.wap.connectivity-wbxml",
            "wsp_tid=0x01 pdu=0x06",
            "NAPDEF",
            "NAPID: internet",
            "NAME: Telekom",
        ] {
            assert!(
                formatted.contains(expected),
                "missing {expected:?}: {formatted}"
            );
        }
    }

    #[test]
    fn classifies_wap_si_and_mms_and_extracts_safe_hints() {
        let si = wsp(b"application/vnd.wap.sic", b"title\0https://example.com/si");
        let formatted = format(&message(0, 4, si)).unwrap();
        assert!(formatted.contains("[WAP SI Push]"));
        assert!(formatted.contains("url=https://example.com/si"));

        let mut mms = wsp(
            b"application/vnd.wap.mms-message",
            &[
                0x8c, 0x82, 0x98, b't', b'x', b'1', 0, 0x83, b'h', b't', b't', b'p', b':', b'/',
                b'/', b'm', 0, 0x8e, 0x81, 0x48,
            ],
        );
        let formatted = format(&message(0, 4, std::mem::take(&mut mms))).unwrap();
        assert!(formatted.contains("[MMS Notification]"));
        assert!(formatted.contains("x_mms_transaction_id=tx1"));
        assert!(formatted.contains("message_size=200"));
    }

    #[test]
    fn identifies_sim_ota_without_claiming_decryption() {
        let formatted =
            format(&message(0x7f, 4, vec![0, 5, 0x11, 0x22, 0x33, 0x44, 0x55])).unwrap();
        assert!(formatted.contains("[SIM OTA 23.048]"));
        assert!(formatted.contains("spi=0x1122"));
        assert!(formatted.contains("decrypt=not_attempted"));
        assert!(formatted.contains("security=可能加密"));
    }
}
