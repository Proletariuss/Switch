//! MESSAGE identity and ordered routes learned only from the completed REGISTER.
use proletarius_ims::SipResponse;
use zeroize::Zeroizing;

/// Owned signaling snapshot, captured before the registered socket moves to its actor.
/// No socket or SA lease is duplicated by this metadata object.
pub struct RegisteredMessageContext {
    routing: RegisteredMessageRouting,
    local_ip: std::net::IpAddr,
    local_port: u16,
    security_verify: Option<Zeroizing<String>>,
}
impl RegisteredMessageContext {
    pub(crate) fn new(
        routing: RegisteredMessageRouting,
        local_ip: std::net::IpAddr,
        local_port: u16,
        security_verify: Option<&str>,
    ) -> Self {
        Self {
            routing,
            local_ip,
            local_port,
            security_verify: security_verify.map(|value| Zeroizing::new(value.to_owned())),
        }
    }
    pub fn routing(&self) -> &RegisteredMessageRouting {
        &self.routing
    }
    pub fn local_ip(&self) -> std::net::IpAddr {
        self.local_ip
    }
    pub fn local_port(&self) -> u16 {
        self.local_port
    }
    pub fn security_verify(&self) -> Option<&str> {
        self.security_verify.as_ref().map(|value| value.as_str())
    }
}

pub struct RegisteredMessageRouting {
    identity: Zeroizing<String>,
    routes: Zeroizing<Vec<String>>,
    registered_msisdn: Zeroizing<String>,
}

impl RegisteredMessageRouting {
    pub fn from_headers(
        associated_uri_headers: &[String],
        service_route_headers: &[String],
        configured_identity: &str,
        fallback_routes: &[String],
    ) -> Result<Self, &'static str> {
        let mut first = None;
        let mut selected = None;
        for header in associated_uri_headers {
            for value in split_values(header) {
                if let Some(identity) = identity_uri(value) {
                    if first.is_none() {
                        first = Some(identity);
                    }
                    if identity == configured_identity {
                        selected = Some(identity);
                    }
                }
            }
        }
        let identity = selected.or(first).unwrap_or(configured_identity);
        let registered_msisdn = associated_uri_headers
            .iter()
            .flat_map(|header| split_values(header))
            .find_map(normalize_associated_msisdn)
            .unwrap_or_default();
        if identity_uri(identity).is_none() {
            return Err("invalid_registered_message_identity");
        }
        let mut routes: Vec<String> = service_route_headers
            .iter()
            .map(|route| route.trim())
            .filter(|route| !route.is_empty())
            .map(str::to_owned)
            .collect();
        if routes.is_empty() {
            routes = fallback_routes.to_vec();
        }
        if routes.is_empty()
            || routes.len() > 8
            || routes.iter().any(|route| {
                route.is_empty()
                    || route.len() > 4096
                    || route.bytes().any(|byte| byte.is_ascii_control())
            })
        {
            return Err("invalid_registered_message_routes");
        }
        Ok(Self {
            identity: Zeroizing::new(identity.to_owned()),
            routes: Zeroizing::new(routes),
            registered_msisdn: Zeroizing::new(registered_msisdn),
        })
    }

    pub fn from_response(
        response: &SipResponse,
        configured_identity: &str,
        fallback_routes: &[String],
    ) -> Result<Self, &'static str> {
        if !(200..=299).contains(&response.status) {
            return Err("registration_not_successful");
        }
        let associated: Vec<String> = response
            .headers("P-Associated-URI")
            .map(str::to_owned)
            .collect();
        let routes: Vec<String> = response
            .headers("Service-Route")
            .map(str::to_owned)
            .collect();
        Self::from_headers(&associated, &routes, configured_identity, fallback_routes)
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn routes(&self) -> &[String] {
        &self.routes
    }
    pub fn registered_msisdn(&self) -> Option<&str> {
        (!self.registered_msisdn.is_empty()).then_some(self.registered_msisdn.as_str())
    }
}

fn normalize_associated_msisdn(raw: &str) -> Option<String> {
    let mut value = raw.trim().trim_matches(['<', '>', '"', '\'']);
    if value
        .get(..4)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("tel:"))
    {
        value = &value[4..];
    } else if value
        .get(..4)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("sip:"))
        || value
            .get(..5)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("sips:"))
    {
        let prefix = if value[..4].eq_ignore_ascii_case("sip:") {
            4
        } else {
            5
        };
        value = &value[prefix..];
        if let Some((user, _)) = value.split_once('@') {
            value = user;
        }
        if !value.trim().starts_with('+') {
            return None;
        }
    } else {
        return None;
    }
    if let Some(index) = value.find([';', '?']) {
        value = &value[..index];
    }
    let decoded = percent_decode_path(value).unwrap_or_else(|| value.to_owned());
    let mut normalized = String::new();
    for character in decoded.trim().chars() {
        match character {
            '+' if normalized.is_empty() => normalized.push('+'),
            '0'..='9' => normalized.push(character),
            character if character.is_whitespace() || matches!(character, '-' | '(' | ')') => {}
            _ => return None,
        }
    }
    let digits = normalized.strip_prefix('+')?;
    (5..=15).contains(&digits.len()).then_some(normalized)
}

fn percent_decode_path(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex_value(high)? << 4 | hex_value(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn identity_uri(value: &str) -> Option<&str> {
    if value.contains(['\r', '\n']) {
        return None;
    }
    let mut value = value.trim();
    if let Some((_, rest)) = value.split_once('<') {
        value = rest.split_once('>')?.0;
    }
    if value.contains(['<', '>', '"', ' ', '\t', ',']) {
        return None;
    }
    let (scheme, rest) = value.split_once(':')?;
    if rest.is_empty() {
        return None;
    }
    if scheme.eq_ignore_ascii_case("sip") || scheme.eq_ignore_ascii_case("sips") {
        if !rest.contains('@') {
            return None;
        }
    } else if scheme.eq_ignore_ascii_case("tel") {
        if !rest.starts_with('+') {
            return None;
        }
    } else {
        return None;
    }
    Some(value)
}

fn split_values(value: &str) -> Vec<&str> {
    let (mut start, mut depth, mut quoted) = (0, 0usize, false);
    let mut values = Vec::new();
    for (index, character) in value.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '<' if !quoted => depth += 1,
            '>' if !quoted => depth = depth.saturating_sub(1),
            ',' if !quoted && depth == 0 => {
                values.push(value[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    values.push(value[start..].trim());
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(status: u16, headers: &str) -> SipResponse {
        SipResponse::parse(
            format!("SIP/2.0 {status} Fixture\r\n{headers}Content-Length: 0\r\n\r\n").as_bytes(),
        )
        .unwrap()
    }
    #[test]
    fn successful_registration_prefers_authorized_configured_identity_and_keeps_route_order() {
        let response = response(
            200,
            "P-Associated-URI: <tel:+447700900123>, \"Name, label\" <tel:+447700900123>, <sip:configured@ims.example>\r\nService-Route: <sip:first;lr>, <sip:second;lr>\r\nService-Route: <sip:third;lr>\r\n",
        );
        let routing = RegisteredMessageRouting::from_response(
            &response,
            "sip:configured@ims.example",
            &["<sip:fallback;lr>".into()],
        )
        .unwrap();
        assert_eq!(routing.identity(), "sip:configured@ims.example");
        assert_eq!(routing.registered_msisdn(), Some("+447700900123"));
        assert_eq!(
            routing.routes(),
            &["<sip:first;lr>, <sip:second;lr>", "<sip:third;lr>"]
        );
        let routing =
            RegisteredMessageRouting::from_response(&response, "sip:unlisted@ims.example", &[])
                .unwrap();
        assert_eq!(routing.identity(), "tel:+447700900123");
    }
    #[test]
    fn missing_metadata_uses_configured_fallback_but_non_success_never_produces_snapshot() {
        let fallback = vec!["<sip:fallback;lr>".into()];
        let routing = RegisteredMessageRouting::from_response(
            &response(200, "P-Associated-URI: invalid\r\n"),
            "sip:configured@ims.example",
            &fallback,
        )
        .unwrap();
        assert_eq!(routing.identity(), "sip:configured@ims.example");
        assert_eq!(routing.routes(), fallback);
        assert!(
            RegisteredMessageRouting::from_response(
                &response(403, ""),
                "sip:a@ims.example",
                &fallback
            )
            .is_err()
        );
        assert!(
            RegisteredMessageRouting::from_response(&response(200, ""), "invalid", &fallback)
                .is_err()
        );
        assert!(
            RegisteredMessageRouting::from_response(&response(200, ""), "sip:a@ims.example", &[])
                .is_err()
        );
    }

    #[test]
    fn associated_msisdn_accepts_only_typed_e164_identities() {
        let cases = [
            ("<tel:+44 7700-900123>", Some("+447700900123")),
            (
                "<sip:+447700900123@ims.example;user=phone>",
                Some("+447700900123"),
            ),
            ("tel:%2B447700900123", Some("+447700900123")),
            ("sip:234150000000000@ims.example", None),
            ("+447700900123", None),
            ("<tel:+1234>", None),
            ("<tel:+1234567890123456>", None),
        ];
        for (header, expected) in cases {
            let projected = RegisteredMessageRouting::from_headers(
                &[header.to_owned()],
                &["<sip:route;lr>".to_owned()],
                "sip:configured@ims.example",
                &[],
            )
            .unwrap();
            assert_eq!(projected.registered_msisdn(), expected, "header {header:?}");
        }
    }
}
