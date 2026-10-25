//! Ordered REGISTER serialization. Carrier policy supplies header values and
//! order; this layer owns framing and rejects injection/duplicate core fields.
use crate::{SipError, aka_authorization::AuthorizationHeader};
use std::net::IpAddr;
use zeroize::Zeroizing;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterTransportHeaders {
    pub via: String,
    pub call_id: String,
    pub cseq: String,
    pub max_forwards: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialRegisterSecurityDecision {
    pub require_ipsec: bool,
    pub plain_ok: bool,
    pub reason: &'static str,
    pub security_server: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengeSecurityDecision {
    pub install_ipsec: bool,
    pub require_verify: bool,
    pub missing_security_client: bool,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeSecurityError {
    MissingSecurityServer,
    MissingSecurityClientMechanisms,
}

/// Selects the existing sec-agree outcome after a 401/407 challenge.
/// Installation of IPsec and request mutation remain owned by the session.
pub fn decide_challenge_security(
    has_security_server: bool,
    sec_agree_enabled: bool,
    sec_agree_required: bool,
    security_client_mechanism_count: usize,
) -> Result<ChallengeSecurityDecision, ChallengeSecurityError> {
    if !sec_agree_enabled {
        return Ok(ChallengeSecurityDecision {
            install_ipsec: false,
            require_verify: false,
            missing_security_client: false,
            reason: "sec_agree_disabled",
        });
    }
    if !has_security_server {
        if sec_agree_required {
            return Err(ChallengeSecurityError::MissingSecurityServer);
        }
        return Ok(ChallengeSecurityDecision {
            install_ipsec: false,
            require_verify: false,
            missing_security_client: false,
            reason: "security_server_missing_auto_disable",
        });
    }
    if security_client_mechanism_count == 0 {
        return Err(ChallengeSecurityError::MissingSecurityClientMechanisms);
    }
    Ok(ChallengeSecurityDecision {
        install_ipsec: true,
        require_verify: true,
        missing_security_client: false,
        reason: "security_server_offer_selected",
    })
}

/// Applies the existing policy for a successful unauthenticated REGISTER.
/// Transport/IPsec side effects remain with the session owner.
pub fn decide_initial_register_security(
    status: u16,
    has_security_server: bool,
    sec_agree_enabled: bool,
    sec_agree_required: bool,
) -> Result<InitialRegisterSecurityDecision, crate::SipError> {
    if status != 200 {
        return Err(crate::SipError::InvalidTransition);
    }
    match (has_security_server, sec_agree_enabled) {
        (true, true) => Ok(InitialRegisterSecurityDecision {
            require_ipsec: true,
            plain_ok: false,
            reason: "initial_200_security_server_without_ipsec_install_required",
            security_server: true,
        }),
        (true, false) => Ok(InitialRegisterSecurityDecision {
            require_ipsec: false,
            plain_ok: false,
            reason: "initial_200_security_server_ignored_without_ipsec_install",
            security_server: true,
        }),
        (false, true) if sec_agree_required => Err(crate::SipError::MissingSecurityServer),
        (false, true) => Ok(InitialRegisterSecurityDecision {
            require_ipsec: false,
            plain_ok: true,
            reason: "initial_200_without_security_server_auto_plain",
            security_server: false,
        }),
        (false, false) => Ok(InitialRegisterSecurityDecision {
            require_ipsec: false,
            plain_ok: true,
            reason: "initial_200_plain",
            security_server: false,
        }),
    }
}

/// Builds the transaction headers attached by the REGISTER transport.
/// The sequence and Call-ID remain session-owned inputs; branch entropy and
/// wire formatting are owned here.
pub fn build_transport_headers(
    transport: &str,
    local_ip: &str,
    local_port: u16,
    call_id: &str,
    cseq: u32,
) -> Result<RegisterTransportHeaders, SipError> {
    let transport = transport.trim().to_ascii_uppercase();
    if transport.is_empty()
        || transport.len() > 16
        || !transport.bytes().all(crate::is_header_name_byte)
        || local_port == 0
        || cseq == 0
        || call_id.trim().is_empty()
        || call_id.len() > 256
        || call_id
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(SipError::InvalidHeader);
    }
    let ip = if local_ip.trim().is_empty() {
        IpAddr::from([127, 0, 0, 1])
    } else {
        local_ip
            .trim()
            .parse::<IpAddr>()
            .map_err(|_| SipError::InvalidHeader)?
    };
    let host = match ip {
        IpAddr::V4(ip) => format!("{ip}:{local_port}"),
        IpAddr::V6(ip) => format!("[{ip}]:{local_port}"),
    };

    const ALPHANUMERIC: &[u8; 62] =
        b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut branch_suffix = String::with_capacity(16);
    while branch_suffix.len() < 16 {
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| SipError::EntropyUnavailable)?;
        for byte in random {
            // 248 is the largest multiple of 62 below 256, avoiding modulo bias.
            if byte < 248 && branch_suffix.len() < 16 {
                branch_suffix.push(ALPHANUMERIC[(byte % 62) as usize] as char);
            }
        }
    }
    let branch = format!("z9hG4bK.{branch_suffix}");
    Ok(RegisterTransportHeaders {
        via: format!("SIP/2.0/{transport} {host};branch={branch}"),
        call_id: call_id.to_owned(),
        cseq: format!("{cseq} REGISTER"),
        max_forwards: "70",
    })
}

pub struct RegisterRequest<'a> {
    pub uri: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
}

impl RegisterRequest<'_> {
    pub fn encode(
        &self,
        authorization: Option<&AuthorizationHeader>,
    ) -> Result<Zeroizing<Vec<u8>>, SipError> {
        if !(self.uri.starts_with("sip:") || self.uri.starts_with("sips:"))
            || self.uri.len() > 4096
            || self
                .uri
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err(SipError::InvalidHeader);
        }
        if self.headers.len() > 120 || self.body.len() > 65_535 {
            return Err(SipError::MessageTooLarge);
        }
        let mut out = Zeroizing::new(format!("REGISTER {} SIP/2.0\r\n", self.uri).into_bytes());
        let mut core = [false; 7];
        for &(name, value) in self.headers {
            validate(name, value)?;
            let key = name.to_ascii_lowercase();
            if matches!(key.as_str(), "content-length" | "l") {
                return Err(SipError::DuplicateContentLength);
            }
            if authorization.is_some_and(|auth| name.eq_ignore_ascii_case(auth.name())) {
                return Err(SipError::InvalidHeader);
            }
            let index = match key.as_str() {
                "via" | "v" => Some(0),
                "from" | "f" => Some(1),
                "to" | "t" => Some(2),
                "call-id" | "i" => Some(3),
                "cseq" => Some(4),
                "max-forwards" => Some(5),
                "contact" | "m" => Some(6),
                _ => None,
            };
            if let Some(index) = index {
                if core[index] {
                    return Err(SipError::InvalidHeader);
                }
                core[index] = true;
            }
            append(&mut out, name, value);
            if out.len() > 65_536 {
                return Err(SipError::MessageTooLarge);
            }
        }
        if core.iter().any(|present| !present) {
            return Err(SipError::InvalidHeader);
        }
        if let Some(auth) = authorization {
            if !matches!(auth.name(), "Authorization" | "Proxy-Authorization") {
                return Err(SipError::InvalidHeader);
            }
            validate(auth.name(), auth.value())?;
            append(&mut out, auth.name(), auth.value());
        }
        let content_length = self.body.len().to_string();
        let final_length = out
            .len()
            .checked_add("Content-Length: ".len())
            .and_then(|length| length.checked_add(content_length.len()))
            .and_then(|length| length.checked_add(4))
            .and_then(|length| length.checked_add(self.body.len()))
            .ok_or(SipError::MessageTooLarge)?;
        if final_length > 65_535 {
            return Err(SipError::MessageTooLarge);
        }
        append(&mut out, "Content-Length", &content_length);
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(self.body);
        // Use the same transaction validator as the actual sending path.
        crate::registration_connection::request_transaction(&out)?;
        Ok(out)
    }
}

fn validate(name: &str, value: &str) -> Result<(), SipError> {
    if name.is_empty()
        || !name.bytes().all(crate::is_header_name_byte)
        || value.trim().is_empty()
        || value.bytes().any(|b| b.is_ascii_control())
    {
        return Err(SipError::InvalidHeader);
    }
    if name.len() + value.len() + 2 > 8192 {
        return Err(SipError::HeaderLineTooLong);
    }
    Ok(())
}
fn append(out: &mut Vec<u8>, name: &str, value: &str) {
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(value.as_bytes());
    out.extend_from_slice(b"\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_register_security_policy_preserves_plain_and_ipsec_decisions() {
        let cases = [
            (
                true,
                true,
                true,
                false,
                true,
                "initial_200_security_server_without_ipsec_install_required",
            ),
            (
                true,
                false,
                false,
                false,
                true,
                "initial_200_security_server_ignored_without_ipsec_install",
            ),
            (
                false,
                true,
                false,
                true,
                false,
                "initial_200_without_security_server_auto_plain",
            ),
            (false, false, false, true, false, "initial_200_plain"),
        ];
        for (has_server, enabled, expected_ipsec, expected_plain, expected_server, reason) in cases
        {
            let decision =
                decide_initial_register_security(200, has_server, enabled, false).unwrap();
            assert_eq!(decision.require_ipsec, expected_ipsec);
            assert_eq!(decision.security_server, expected_server);
            assert_eq!(decision.reason, reason);
            assert_eq!(decision.plain_ok, expected_plain);
        }
        assert_eq!(
            decide_initial_register_security(200, false, true, true),
            Err(crate::SipError::MissingSecurityServer)
        );
        assert_eq!(
            decide_initial_register_security(403, false, false, false),
            Err(crate::SipError::InvalidTransition)
        );
    }

    #[test]
    fn challenge_security_policy_preserves_disabled_auto_required_and_missing_client_cases() {
        let disabled = decide_challenge_security(true, false, false, 0).unwrap();
        assert_eq!(disabled.reason, "sec_agree_disabled");
        assert!(
            !disabled.install_ipsec
                && !disabled.require_verify
                && !disabled.missing_security_client
        );

        let automatic = decide_challenge_security(false, true, false, 1).unwrap();
        assert_eq!(automatic.reason, "security_server_missing_auto_disable");
        assert!(!automatic.install_ipsec && !automatic.require_verify);

        assert_eq!(
            decide_challenge_security(false, true, true, 1),
            Err(ChallengeSecurityError::MissingSecurityServer)
        );
        assert_eq!(
            decide_challenge_security(true, true, false, 0),
            Err(ChallengeSecurityError::MissingSecurityClientMechanisms)
        );

        let selected = decide_challenge_security(true, true, true, 2).unwrap();
        assert_eq!(selected.reason, "security_server_offer_selected");
        assert!(selected.install_ipsec && selected.require_verify);
        assert!(!selected.missing_security_client);
    }

    #[test]
    fn transport_headers_preserve_transaction_values_and_format_ipv4_ipv6() {
        for (ip, expected_host) in [
            ("192.0.2.7", "192.0.2.7:5060"),
            ("2001:db8::7", "[2001:db8::7]:5060"),
            ("", "127.0.0.1:5060"),
        ] {
            let headers = build_transport_headers("tcp", ip, 5060, "session@example", 42).unwrap();
            assert!(
                headers
                    .via
                    .starts_with(&format!("SIP/2.0/TCP {expected_host};branch=z9hG4bK."))
            );
            let branch = headers.via.split(";branch=").nth(1).unwrap();
            assert_eq!(branch.len(), 24);
            assert!(branch[8..].bytes().all(|byte| byte.is_ascii_alphanumeric()));
            assert_eq!(headers.call_id, "session@example");
            assert_eq!(headers.cseq, "42 REGISTER");
            assert_eq!(headers.max_forwards, "70");
        }
    }

    #[test]
    fn transport_headers_reject_invalid_session_inputs() {
        assert!(build_transport_headers("tcp\r\nBad", "192.0.2.1", 5060, "id", 1).is_err());
        assert!(build_transport_headers("tcp", "bad ip", 5060, "id", 1).is_err());
        assert!(build_transport_headers("tcp", "192.0.2.1", 5060, "id\nVia:x", 1).is_err());
        assert!(build_transport_headers("tcp", "192.0.2.1", 5060, "id", 0).is_err());
    }
    #[test]
    fn preserves_template_order_and_frames_binary_body() {
        let mut headers = vec![
            ("Via", "SIP/2.0/TCP 10.0.0.2:5060;branch=z9hG4bK-one"),
            ("From", "<sip:u@ims.test>;tag=one"),
            ("To", "<sip:u@ims.test>"),
            ("Call-ID", "reg-1"),
            ("CSeq", "1 REGISTER"),
            ("Max-Forwards", "70"),
            ("Contact", "<sip:u@10.0.0.2:5060;transport=tcp>"),
            ("Supported", "path,sec-agree,gruu"),
        ];
        let auth = AuthorizationHeader::from_parts("Authorization", "Digest username=\"u\"");
        let wire = RegisterRequest {
            uri: "sip:ims.test",
            headers: &headers,
            body: b"a\0b",
        }
        .encode(Some(&auth))
        .unwrap();
        assert!(wire.ends_with(b"Supported: path,sec-agree,gruu\r\nAuthorization: Digest username=\"u\"\r\nContent-Length: 3\r\n\r\na\0b"));
        for extra in [
            ("l", "3"),
            ("i", "other"),
            ("X-Test", "ok\r\nInjected: yes"),
            ("Authorization", "Digest nonce=x"),
        ] {
            headers.push(extra);
            assert!(
                RegisterRequest {
                    uri: "sip:ims.test",
                    headers: &headers,
                    body: b""
                }
                .encode(Some(&auth))
                .is_err()
            );
            headers.pop();
        }
    }
}
