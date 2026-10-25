//! Small XML codec for IMS USSD/USSI signaling.
//!
//! Dialog ownership and SIP transactions remain in the session runtime. This
//! module owns only the bounded user text validation and wire representation.

use std::{fmt, net::IpAddr};

use quick_xml::{events::Event, reader::Reader};

const XML_HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UssdXmlError {
    InvalidText,
    InvalidAddress,
    EntropyUnavailable,
    InvalidXml,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UssdContentError {
    ResponseTooLarge,
    InvalidContentType,
    MissingContent,
    UnsupportedContentType,
    InvalidXml,
    CarrierError,
}

/// Decodes a bounded USSD response from the SIP Content-Type and body. This
/// moves MIME selection and part extraction to Rust while leaving SIP framing
/// and dialog I/O with the transport owner.
pub fn decode_content(
    content_type: &str,
    body: &[u8],
) -> Result<UssdXmlDocument, UssdContentError> {
    if body.len() > 16 * 1024 {
        return Err(UssdContentError::ResponseTooLarge);
    }
    if content_type.is_empty()
        || content_type.len() > 4096
        || content_type
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | '\0'))
    {
        return Err(UssdContentError::InvalidContentType);
    }
    let strict_type: mime::Mime = content_type
        .parse()
        .map_err(|_| UssdContentError::InvalidContentType)?;
    let parsed_type = mailparse::parse_content_type(content_type);
    if parsed_type.mimetype == "application/vnd.3gpp.ussd+xml" {
        return finish_content(decode(body).map_err(|_| UssdContentError::InvalidXml)?);
    }
    if strict_type.type_() != mime::MULTIPART || strict_type.subtype() != "mixed" {
        return Err(UssdContentError::UnsupportedContentType);
    }
    if !parsed_type.params.contains_key("boundary") {
        return Err(UssdContentError::InvalidContentType);
    }

    let mut message = Vec::with_capacity(content_type.len() + body.len() + 32);
    message.extend_from_slice(b"Content-Type: ");
    message.extend_from_slice(content_type.as_bytes());
    message.extend_from_slice(b"\r\n\r\n");
    message.extend_from_slice(body);
    let parsed =
        mailparse::parse_mail(&message).map_err(|_| UssdContentError::InvalidContentType)?;
    for part in parsed.subparts.iter().take(8) {
        if part.ctype.mimetype != "application/vnd.3gpp.ussd+xml" {
            continue;
        }
        let transfer_encoding = part
            .headers
            .iter()
            .find(|header| {
                header
                    .get_key()
                    .eq_ignore_ascii_case("Content-Transfer-Encoding")
            })
            .map(|header| header.get_value().trim().to_ascii_lowercase());
        // Go's multipart.Part transparently decodes quoted-printable but does
        // not decode a base64 transfer encoding. Preserve that wire behavior.
        if matches!(transfer_encoding.as_deref(), Some("base64")) {
            return Err(UssdContentError::InvalidXml);
        }
        let document = part
            .get_body_raw()
            .map_err(|_| UssdContentError::InvalidXml)?;
        if document.len() > 16 * 1024 {
            return Err(UssdContentError::ResponseTooLarge);
        }
        return finish_content(decode(&document).map_err(|_| UssdContentError::InvalidXml)?);
    }
    Err(UssdContentError::MissingContent)
}

fn finish_content(result: UssdXmlDocument) -> Result<UssdXmlDocument, UssdContentError> {
    if !result.error_code.is_empty() {
        return Err(UssdContentError::CarrierError);
    }
    Ok(result)
}

impl fmt::Display for UssdXmlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidText => {
                formatter.write_str("USSD text must contain 1 to 182 valid characters")
            }
            Self::InvalidAddress => formatter.write_str("USSD local address is invalid"),
            Self::EntropyUnavailable => formatter.write_str("USSD boundary entropy is unavailable"),
            Self::InvalidXml => formatter.write_str("USSD XML is invalid"),
        }
    }
}

impl std::error::Error for UssdXmlError {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UssdXmlDocument {
    pub language: String,
    pub text: String,
    pub error_code: String,
}

/// Decodes the bounded USSD XML fields consumed by the IMS dialog runtime.
/// Unknown fields are ignored, as with the prior Go `encoding/xml` struct.
pub fn decode(document: &[u8]) -> Result<UssdXmlDocument, UssdXmlError> {
    if document.len() > 16 * 1024 {
        return Err(UssdXmlError::InvalidXml);
    }
    let source = std::str::from_utf8(document).map_err(|_| UssdXmlError::InvalidXml)?;
    let mut reader = Reader::from_str(source);
    reader.config_mut().trim_text(false);
    let mut result = UssdXmlDocument::default();
    let mut stack = Vec::<Vec<u8>>::new();
    let mut active: Option<(Field, String)> = None;
    let mut root_seen = false;

    loop {
        let event = reader.read_event().map_err(|_| UssdXmlError::InvalidXml)?;
        match event {
            Event::Start(element) => {
                let name = element.local_name().as_ref().to_vec();
                if stack.is_empty() {
                    if root_seen || name != b"ussd-data" {
                        return Err(UssdXmlError::InvalidXml);
                    }
                    root_seen = true;
                } else if stack.len() == 1 {
                    active = Field::from_name(&name).map(|field| (field, String::new()));
                }
                stack.push(name);
            }
            Event::Empty(element) => {
                let name = element.local_name().as_ref().to_vec();
                if stack.is_empty() {
                    if root_seen || name != b"ussd-data" {
                        return Err(UssdXmlError::InvalidXml);
                    }
                    root_seen = true;
                } else if stack.len() == 1 {
                    if let Some(field) = Field::from_name(&name) {
                        field.assign(&mut result, String::new());
                    }
                }
            }
            Event::Text(text) if active.is_some() && stack.len() == 2 => {
                let raw =
                    std::str::from_utf8(text.as_ref()).map_err(|_| UssdXmlError::InvalidXml)?;
                let decoded =
                    quick_xml::escape::unescape(raw).map_err(|_| UssdXmlError::InvalidXml)?;
                active
                    .as_mut()
                    .expect("active field was checked")
                    .1
                    .push_str(&normalize_eol(&decoded));
            }
            Event::CData(text) if active.is_some() && stack.len() == 2 => {
                let decoded = text.decode().map_err(|_| UssdXmlError::InvalidXml)?;
                active
                    .as_mut()
                    .expect("active field was checked")
                    .1
                    .push_str(&normalize_eol(&decoded));
            }
            Event::GeneralRef(reference) if active.is_some() && stack.len() == 2 => {
                let entity = std::str::from_utf8(reference.as_ref())
                    .map_err(|_| UssdXmlError::InvalidXml)?;
                let character = reference
                    .resolve_char_ref()
                    .map_err(|_| UssdXmlError::InvalidXml)?
                    .map(String::from)
                    .or_else(|| {
                        quick_xml::escape::resolve_predefined_entity(entity).map(str::to_owned)
                    })
                    .ok_or(UssdXmlError::InvalidXml)?;
                active
                    .as_mut()
                    .expect("active field was checked")
                    .1
                    .push_str(&character);
            }
            Event::End(element) => {
                if stack.is_empty() {
                    return Err(UssdXmlError::InvalidXml);
                }
                if stack.len() == 2 {
                    if let Some((field, value)) = active.take() {
                        if field.matches(element.local_name().as_ref()) {
                            field.assign(&mut result, value);
                        }
                    }
                }
                let previous = stack.pop().expect("non-empty stack checked");
                if previous.as_slice() != element.local_name().as_ref() {
                    return Err(UssdXmlError::InvalidXml);
                }
            }
            Event::Eof => break,
            Event::DocType(_) | Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
            _ => {}
        }
    }
    if !root_seen || !stack.is_empty() || active.is_some() {
        return Err(UssdXmlError::InvalidXml);
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Field {
    Language,
    Text,
    ErrorCode,
}

impl Field {
    fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"language" => Some(Self::Language),
            b"ussd-string" => Some(Self::Text),
            b"error-code" => Some(Self::ErrorCode),
            _ => None,
        }
    }

    fn matches(self, name: &[u8]) -> bool {
        Self::from_name(name) == Some(self)
    }

    fn assign(self, target: &mut UssdXmlDocument, value: String) {
        match self {
            Self::Language => target.language = value,
            Self::Text => target.text = value,
            Self::ErrorCode => target.error_code = value,
        }
    }
}

fn normalize_eol(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

/// Encodes a USSD string using the existing Go `encoding/xml` wire shape.
pub fn encode(text: &str) -> Result<Vec<u8>, UssdXmlError> {
    let char_count = text.chars().count();
    if char_count == 0
        || char_count > 182
        || text
            .chars()
            .any(|character| matches!(character, '\0' | '\r' | '\n'))
    {
        return Err(UssdXmlError::InvalidText);
    }

    let mut output = String::with_capacity(XML_HEADER.len() + text.len() + 80);
    output.push_str(XML_HEADER);
    output.push_str("<ussd-data><language>en</language><ussd-string>");
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&#34;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
    output.push_str("</ussd-string></ussd-data>");
    Ok(output.into_bytes())
}

/// Builds the signaling-only SDP and USSD multipart body used by the reader
/// IMS runtime. SIP headers, transactions, and socket writes remain outside
/// this codec. The boundary is fresh for every body and contains only MIME
/// token characters.
pub fn build_invite_body(text: &str, local_ip: &str) -> Result<(Vec<u8>, String), UssdXmlError> {
    let address: IpAddr = local_ip.parse().map_err(|_| UssdXmlError::InvalidAddress)?;
    let family = match address {
        IpAddr::V4(_) => "IP4",
        IpAddr::V6(_) => "IP6",
    };
    let xml = encode(text)?;

    let mut entropy = [0_u8; 24];
    getrandom::fill(&mut entropy).map_err(|_| UssdXmlError::EntropyUnavailable)?;
    let boundary = format!("switch-ussd-{}", lower_hex(&entropy));
    let content_type = format!("multipart/mixed; boundary={boundary}");

    let sdp = format!(
        "v=0\r\no=- 1 1 IN {family} {local_ip}\r\ns=-\r\nc=IN {family} {local_ip}\r\nt=0 0\r\nm=audio 0 RTP/AVP 98\r\na=rtpmap:98 AMR-WB/16000\r\na=inactive\r\n"
    );
    let mut body = Vec::with_capacity(sdp.len() + xml.len() + boundary.len() * 3 + 160);
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Type: application/sdp\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(sdp.as_bytes());
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: render;handling=optional\r\nContent-Type: application/vnd.3gpp.ussd+xml\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&xml);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok((body, content_type))
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_existing_go_xml_wire_shape_and_escaping() {
        assert_eq!(
            encode("余额 & <信息> \"'").unwrap(),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ussd-data><language>en</language><ussd-string>余额 &amp; &lt;信息&gt; &#34;&#39;</ussd-string></ussd-data>".as_bytes()
        );
    }

    #[test]
    fn validates_character_count_and_rejects_wire_controls() {
        for value in ["", "a\r", "a\n", "a\0", &"a".repeat(183)] {
            assert_eq!(encode(value), Err(UssdXmlError::InvalidText));
        }
        assert!(encode(&"界".repeat(182)).is_ok());
        assert!(encode("##107#").is_ok());
    }

    #[test]
    fn builds_signaling_only_ipv4_and_ipv6_multipart_bodies() {
        for (address, family) in [("192.0.2.1", "IP4"), ("2001:db8::1", "IP6")] {
            let (body, content_type) = build_invite_body("##107#", address).unwrap();
            assert!(content_type.starts_with("multipart/mixed; boundary=switch-ussd-"));
            let boundary = content_type
                .strip_prefix("multipart/mixed; boundary=")
                .unwrap();
            assert_eq!(boundary.len(), "switch-ussd-".len() + 48);
            assert!(body.starts_with(
                format!("--{boundary}\r\nContent-Type: application/sdp\r\n\r\n").as_bytes()
            ));
            assert!(
                String::from_utf8_lossy(&body).contains(&format!("o=- 1 1 IN {family} {address}"))
            );
            assert!(String::from_utf8_lossy(&body).contains("a=inactive\r\n"));
            assert!(
                String::from_utf8_lossy(&body)
                    .contains("Content-Disposition: render;handling=optional\r\n")
            );
            assert!(String::from_utf8_lossy(&body).ends_with(&format!("\r\n--{boundary}--\r\n")));
        }
    }

    #[test]
    fn rejects_invalid_local_address_and_usss_text() {
        assert_eq!(
            build_invite_body("##107#", "not-an-ip"),
            Err(UssdXmlError::InvalidAddress)
        );
        assert_eq!(
            build_invite_body("", "192.0.2.1"),
            Err(UssdXmlError::InvalidText)
        );
    }

    #[test]
    fn decodes_xml_text_cdata_entities_and_ignores_unknown_fields() {
        let value = decode(
            br#"<?xml version="1.0"?><ussd-data><language>en</language><unknown>ignored</unknown><ussd-string>A &amp; &#35; &#x1F600; <![CDATA[raw <text>]]></ussd-string></ussd-data>"#,
        )
        .unwrap();
        assert_eq!(value.language, "en");
        assert_eq!(value.text, "A & # 😀 raw <text>");
        assert!(value.error_code.is_empty());
    }

    #[test]
    fn decodes_namespaced_root_and_preserves_whitespace_and_error_code() {
        let value = decode(
            r#"<u:ussd-data xmlns:u="urn:ussd"><u:ussd-string> 余额 </u:ussd-string><u:error-code>17</u:error-code></u:ussd-data>"#.as_bytes(),
        )
        .unwrap();
        assert_eq!(value.text, " 余额 ");
        assert_eq!(value.error_code, "17");
    }

    #[test]
    fn rejects_wrong_root_malformed_xml_unknown_entities_and_oversize_documents() {
        for value in [
            b"<wrong/>".as_slice(),
            b"<ussd-data><ussd-string>broken</ussd-data>".as_slice(),
            b"<ussd-data><ussd-string>&unknown;</ussd-string></ussd-data>".as_slice(),
        ] {
            assert_eq!(decode(value), Err(UssdXmlError::InvalidXml));
        }
        assert_eq!(
            decode(&vec![b'a'; 16 * 1024 + 1]),
            Err(UssdXmlError::InvalidXml)
        );
    }

    #[test]
    fn decodes_direct_xml_and_generated_multipart_content() {
        let direct = decode_content(
            "Application/Vnd.3gpp.ussd+xml; charset=utf-8",
            b"<ussd-data><language>en</language><ussd-string>direct &amp; safe</ussd-string></ussd-data>",
        )
        .unwrap();
        assert_eq!(direct.text, "direct & safe");

        let (body, content_type) = build_invite_body("##107#", "192.0.2.1").unwrap();
        let multipart = decode_content(&content_type, &body).unwrap();
        assert_eq!(multipart.text, "##107#");
    }

    #[test]
    fn decodes_quoted_boundary_and_only_searches_first_eight_parts() {
        let boundary = "ussd-boundary";
        let xml = b"<ussd-data><ussd-string>found</ussd-string></ussd-data>";
        let mut body = Vec::new();
        for _ in 0..7 {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Type: application/sdp\r\n\r\nx\r\n").as_bytes(),
            );
        }
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Type: application/vnd.3gpp.ussd+xml\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(xml);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        assert_eq!(
            decode_content(&format!("multipart/mixed; boundary=\"{boundary}\""), &body)
                .unwrap()
                .text,
            "found"
        );

        let mut ninth = Vec::new();
        for _ in 0..8 {
            ninth.extend_from_slice(
                format!("--{boundary}\r\nContent-Type: application/sdp\r\n\r\nx\r\n").as_bytes(),
            );
        }
        ninth.extend_from_slice(
            format!("--{boundary}\r\nContent-Type: application/vnd.3gpp.ussd+xml\r\n\r\n")
                .as_bytes(),
        );
        ninth.extend_from_slice(xml);
        ninth.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        assert_eq!(
            decode_content(&format!("multipart/mixed; boundary={boundary}"), &ninth),
            Err(UssdContentError::MissingContent)
        );
    }

    #[test]
    fn preserves_content_error_categories_and_limits() {
        assert_eq!(
            decode_content("text/plain", b"hello"),
            Err(UssdContentError::UnsupportedContentType)
        );
        assert_eq!(
            decode_content("multipart/mixed", b""),
            Err(UssdContentError::InvalidContentType)
        );
        assert_eq!(
            decode_content(
                "application/vnd.3gpp.ussd+xml",
                b"<ussd-data><error-code>4</error-code></ussd-data>"
            ),
            Err(UssdContentError::CarrierError)
        );
        assert_eq!(
            decode_content("application/vnd.3gpp.ussd+xml", &vec![b'a'; 16 * 1024 + 1]),
            Err(UssdContentError::ResponseTooLarge)
        );
        assert_eq!(
            decode_content("application/vnd.3gpp.ussd+xml", b"<wrong/>"),
            Err(UssdContentError::InvalidXml)
        );
        assert_eq!(
            decode_content("multipart/mixed; boundary=x", b"not a multipart body"),
            Err(UssdContentError::MissingContent)
        );
        assert_eq!(
            decode_content("multipart/mixed; boundary=x\r\nX-Evil: yes", b""),
            Err(UssdContentError::InvalidContentType)
        );
    }
}
