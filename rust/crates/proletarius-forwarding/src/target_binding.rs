//! Go-compatible, secret-safe destination binding for queued SMS deliveries.
//!
//! The stored value is an HMAC of the receiving account identity, not mutable
//! presentation or transport settings. This prevents pending work from
//! silently following changed credentials. The bytes match
//! `smsforward.Forwarder.targetIdentity` in Go.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::StateKey;

type HmacSha256 = Hmac<Sha256>;

/// Opaque digest stored as `destinationMeta.target_identity` in SWF1.
/// Its value is intentionally not printable through `Debug`.
pub struct DestinationBinding(Zeroizing<[u8; 32]>);

impl Clone for DestinationBinding {
    fn clone(&self) -> Self {
        Self(Zeroizing::new(*self.0))
    }
}

impl DestinationBinding {
    /// Computes a binding from an already Go-normalized identity string.
    /// Callers must include the provider prefix and NUL separators exactly as
    /// the Go implementation does; this does not normalize a URL or config.
    pub fn from_go_identity(key: &StateKey, identity: &str) -> Self {
        let mut mac =
            HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts keys of any length");
        mac.update(identity.as_bytes());
        let mut digest = Zeroizing::new([0; 32]);
        digest.copy_from_slice(&mac.finalize().into_bytes());
        Self(digest)
    }

    /// Computes the existing Go Telegram binding:
    /// `HMAC-SHA256(key, "telegram\0" + trim(bot_token) + "\0" + trim(chat_id))`.
    pub fn telegram(key: &StateKey, bot_token: &str, chat_id: &str) -> Self {
        let mut mac =
            HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts keys of any length");
        mac.update(b"telegram\0");
        mac.update(bot_token.trim().as_bytes());
        mac.update(b"\0");
        mac.update(chat_id.trim().as_bytes());
        let mut digest = Zeroizing::new([0; 32]);
        digest.copy_from_slice(&mac.finalize().into_bytes());
        Self(digest)
    }

    /// Computes the legacy Go Bark binding from a server URL and device key.
    /// The server URL follows `url.Parse(...).String()` plus Go's lowercasing
    /// of scheme/host and trimming of trailing `/`; query bytes stay raw.
    pub fn bark(
        key: &StateKey,
        server_url: &str,
        device_key: &str,
    ) -> Result<Self, crate::StateError> {
        let server = normalize_bark_server_url(server_url)?;
        let mut mac =
            HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts keys of any length");
        mac.update(b"bark\0");
        mac.update(server.as_bytes());
        mac.update(b"\0");
        mac.update(device_key.trim().as_bytes());
        let mut digest = Zeroizing::new([0; 32]);
        digest.copy_from_slice(&mac.finalize().into_bytes());
        Ok(Self(digest))
    }

    /// Returns the lowercase hexadecimal representation persisted by Go.
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(64);
        for byte in self.0.iter().copied() {
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 0x0f) as usize] as char);
        }
        output
    }
}

fn normalize_bark_server_url(value: &str) -> Result<String, crate::StateError> {
    let input = value.trim().trim_end_matches('/');
    if input.bytes().any(|byte| byte < b' ' || byte == 0x7f) {
        return Err(crate::StateError::InvalidTargetIdentity);
    }
    let (scheme, rest) = input
        .split_once("://")
        .ok_or(crate::StateError::InvalidTargetIdentity)?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(crate::StateError::InvalidTargetIdentity);
    }

    // Go removes fragment and query before it splits the authority from path.
    let (without_fragment, fragment) = match rest.split_once('#') {
        Some((prefix, suffix)) => (prefix, Some(suffix)),
        None => (rest, None),
    };
    let (authority_and_path, query) = match without_fragment.split_once('?') {
        Some((prefix, suffix)) => (prefix, Some(suffix)),
        None => (without_fragment, None),
    };
    let (authority, path) = match authority_and_path.find('/') {
        Some(index) => (&authority_and_path[..index], &authority_and_path[index..]),
        None => (authority_and_path, ""),
    };
    if authority.is_empty() {
        return Err(crate::StateError::InvalidTargetIdentity);
    }
    let (userinfo, host) = match authority.rsplit_once('@') {
        Some((userinfo, host)) => (Some(normalize_userinfo(userinfo)?), host),
        None => (None, authority),
    };
    let host = normalize_host(host)?;
    let path = normalize_url_component(path, ComponentKind::Path)?;
    let fragment = fragment
        .filter(|fragment| !fragment.is_empty())
        .map(|fragment| normalize_url_component(fragment, ComponentKind::Fragment))
        .transpose()?;

    let mut output = String::with_capacity(input.len());
    output.push_str(&scheme.to_ascii_lowercase());
    output.push_str("://");
    if let Some(userinfo) = userinfo {
        output.push_str(&userinfo);
        output.push('@');
    }
    output.push_str(&host);
    output.push_str(&path);
    if let Some(query) = query {
        output.push('?');
        output.push_str(query);
    }
    if let Some(fragment) = fragment {
        output.push('#');
        output.push_str(&fragment);
    }
    Ok(output)
}

fn normalize_userinfo(value: &str) -> Result<String, crate::StateError> {
    let (username, password) = match value.split_once(':') {
        Some((username, password)) => (username, Some(password)),
        None => (value, None),
    };
    if !value.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || matches!(
                character,
                '-' | '.'
                    | '_'
                    | ':'
                    | '~'
                    | '!'
                    | '$'
                    | '&'
                    | '\''
                    | '('
                    | ')'
                    | '*'
                    | '+'
                    | ','
                    | ';'
                    | '='
                    | '%'
                    | '@'
            )
    }) {
        return Err(crate::StateError::InvalidTargetIdentity);
    }
    let username = percent_decode(username)?;
    let mut output = escape_userinfo(&username);
    if let Some(password) = password {
        output.push(':');
        output.push_str(&escape_userinfo(&percent_decode(password)?));
    }
    Ok(output)
}

fn normalize_host(value: &str) -> Result<String, crate::StateError> {
    if value.is_empty() {
        return Err(crate::StateError::InvalidTargetIdentity);
    }
    if value.starts_with('[') {
        let close = value
            .find(']')
            .ok_or(crate::StateError::InvalidTargetIdentity)?;
        let address = &value[1..close];
        let suffix = &value[close + 1..];
        if !suffix.is_empty()
            && (!suffix.starts_with(':') || !suffix[1..].bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err(crate::StateError::InvalidTargetIdentity);
        }
        validate_raw_ipv6_host(address)?;
        let address = percent_decode(address)?;
        let address =
            String::from_utf8(address).map_err(|_| crate::StateError::InvalidTargetIdentity)?;
        let lower = lowercase_go_simple(&address);
        let (ipv6, zone) = match lower.split_once('%') {
            Some((ipv6, zone)) => (ipv6, Some(zone)),
            None => (lower.as_str(), None),
        };
        ipv6.parse::<std::net::Ipv6Addr>()
            .map_err(|_| crate::StateError::InvalidTargetIdentity)?;
        let mut output = String::from("[");
        output.push_str(ipv6);
        if let Some(zone) = zone {
            output.push_str("%25");
            output.push_str(&escape_host(zone.as_bytes())?);
        }
        output.push(']');
        output.push_str(&suffix.to_ascii_lowercase());
        return Ok(output);
    }

    let (raw_hostname, port) = match value.rsplit_once(':') {
        Some((hostname, port)) if !hostname.contains(':') => {
            if !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(crate::StateError::InvalidTargetIdentity);
            }
            (hostname, Some(port))
        }
        Some(_) => return Err(crate::StateError::InvalidTargetIdentity),
        None => (value, None),
    };
    validate_raw_host(raw_hostname)?;
    let host_bytes = percent_decode(raw_hostname)?;
    let host =
        String::from_utf8(host_bytes).map_err(|_| crate::StateError::InvalidTargetIdentity)?;
    let lower = lowercase_go_simple(&host);
    if lower.is_empty() {
        return Err(crate::StateError::InvalidTargetIdentity);
    }
    let mut output = escape_host(lower.as_bytes())?;
    if let Some(port) = port {
        output.push(':');
        output.push_str(port);
    }
    Ok(output)
}

fn validate_raw_host(value: &str) -> Result<(), crate::StateError> {
    if value.is_empty() {
        return Err(crate::StateError::InvalidTargetIdentity);
    }
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            if index + 2 >= bytes.len() {
                return Err(crate::StateError::InvalidTargetIdentity);
            }
            let high =
                hex_value(bytes[index + 1]).ok_or(crate::StateError::InvalidTargetIdentity)?;
            hex_value(bytes[index + 2]).ok_or(crate::StateError::InvalidTargetIdentity)?;
            // Go's net/url host decoder checks the first hex nibble here.
            if high < 8 && &bytes[index..index + 3] != b"%25" {
                return Err(crate::StateError::InvalidTargetIdentity);
            }
            index += 3;
            continue;
        }
        if byte.is_ascii() && !is_unreserved(byte) && !is_sub_delimiter(byte) {
            return Err(crate::StateError::InvalidTargetIdentity);
        }
        index += 1;
    }
    Ok(())
}

fn validate_raw_ipv6_host(value: &str) -> Result<(), crate::StateError> {
    if value.is_empty() {
        return Err(crate::StateError::InvalidTargetIdentity);
    }
    let mut zone = false;
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            if index + 2 >= bytes.len() {
                return Err(crate::StateError::InvalidTargetIdentity);
            }
            let high =
                hex_value(bytes[index + 1]).ok_or(crate::StateError::InvalidTargetIdentity)?;
            let low =
                hex_value(bytes[index + 2]).ok_or(crate::StateError::InvalidTargetIdentity)?;
            let decoded = (high << 4) | low;
            if !zone {
                if decoded != b'%' || &bytes[index..index + 3] != b"%25" {
                    return Err(crate::StateError::InvalidTargetIdentity);
                }
                zone = true;
            } else if decoded != b'%'
                && decoded != b' '
                && !(decoded.is_ascii_alphanumeric() || is_sub_delimiter(decoded))
            {
                return Err(crate::StateError::InvalidTargetIdentity);
            }
            index += 3;
            continue;
        }
        if !zone {
            if !(byte.is_ascii_hexdigit() || matches!(byte, b':' | b'.')) {
                return Err(crate::StateError::InvalidTargetIdentity);
            }
        } else if byte.is_ascii()
            && !(byte.is_ascii_alphanumeric()
                || is_sub_delimiter(byte)
                || matches!(byte, b':' | b'[' | b']'))
        {
            return Err(crate::StateError::InvalidTargetIdentity);
        }
        index += 1;
    }
    Ok(())
}

fn lowercase_go_simple(value: &str) -> String {
    value
        .chars()
        .map(|character| character.to_lowercase().next().unwrap_or(character))
        .collect()
}

fn normalize_url_component(value: &str, kind: ComponentKind) -> Result<String, crate::StateError> {
    if value.is_ascii() && valid_encoded_component(value, kind) {
        return Ok(value.to_owned());
    }
    let decoded = percent_decode(value)?;
    Ok(escape_component(&decoded, kind))
}

#[derive(Clone, Copy)]
enum ComponentKind {
    Path,
    Fragment,
}

fn valid_encoded_component(value: &str, kind: ComponentKind) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            if index + 2 >= bytes.len()
                || hex_value(bytes[index + 1]).is_none()
                || hex_value(bytes[index + 2]).is_none()
            {
                return false;
            }
            index += 3;
            continue;
        }
        if !is_unreserved(byte)
            && !is_sub_delimiter(byte)
            && !matches!(byte, b':' | b'@' | b'/' | b'[' | b']')
            && !(matches!(kind, ComponentKind::Fragment) && byte == b'?')
        {
            return false;
        }
        index += 1;
    }
    true
}

fn escape_component(bytes: &[u8], kind: ComponentKind) -> String {
    let mut output = String::with_capacity(bytes.len());
    for &byte in bytes {
        let allowed = is_unreserved(byte)
            || is_sub_delimiter(byte)
            || matches!(byte, b':' | b'@' | b'/')
            || (matches!(kind, ComponentKind::Fragment) && byte == b'?');
        if allowed {
            output.push(byte as char);
        } else {
            push_percent_byte(&mut output, byte);
        }
    }
    output
}

fn escape_userinfo(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len());
    for &byte in bytes {
        if is_unreserved(byte) || is_sub_delimiter(byte) || byte == b':' {
            output.push(byte as char);
        } else {
            push_percent_byte(&mut output, byte);
        }
    }
    output
}

fn escape_host(bytes: &[u8]) -> Result<String, crate::StateError> {
    let mut output = String::with_capacity(bytes.len());
    for &byte in bytes {
        if is_unreserved(byte) || is_sub_delimiter(byte) || matches!(byte, b':' | b'[' | b']') {
            output.push(byte as char);
        } else {
            push_percent_byte(&mut output, byte);
        }
    }
    Ok(output)
}

fn percent_decode(value: &str) -> Result<Vec<u8>, crate::StateError> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(crate::StateError::InvalidTargetIdentity);
            }
            let high =
                hex_value(bytes[index + 1]).ok_or(crate::StateError::InvalidTargetIdentity)?;
            let low =
                hex_value(bytes[index + 2]).ok_or(crate::StateError::InvalidTargetIdentity)?;
            output.push((high << 4) | low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    Ok(output)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn push_percent_byte(output: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    output.push('%');
    output.push(HEX[(byte >> 4) as usize] as char);
    output.push(HEX[(byte & 0x0f) as usize] as char);
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn is_sub_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
    )
}

impl std::fmt::Debug for DestinationBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DestinationBinding([REDACTED])")
    }
}
