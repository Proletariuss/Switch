//! Native AKA credential construction. The caller owns transaction routing,
//! nonce counts and fresh cnonce generation; this module never accesses a SIM.
use crate::aka_digest::{AkaDigestInput, AkaPassword, DigestError, DigestQop, compute_aka_digest};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use zeroize::Zeroizing;

/// Parsed fields from one SIP Digest authentication challenge. This keeps
/// challenge interpretation in the Rust IMS protocol owner while callers may
/// still perform the physical AKA APDU through their device adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigestChallenge {
    pub realm: String,
    pub domain: Vec<String>,
    pub nonce: String,
    pub opaque: String,
    pub stale: bool,
    pub algorithm: String,
    pub qop: Vec<String>,
    pub charset: String,
    pub userhash: bool,
}

/// Construct the initial REGISTER Authorization from the current, already
/// verified EAP-AKA result. This consumes no SIM material and never retries
/// AKA; RES remains raw octets for the RFC Digest calculation.
pub fn build_eap_direct_initial_authorization(
    username: &str,
    domain: &str,
    realm: &str,
    res: &[u8],
    rand: &[u8],
    autn: &[u8],
) -> Result<AuthorizationHeader, DigestError> {
    if username.is_empty()
        || domain.is_empty()
        || realm.is_empty()
        || [username, domain, realm]
            .iter()
            .any(|value| value.len() > 4096 || value.bytes().any(|byte| byte.is_ascii_control()))
        || res.is_empty()
        || res.len() > 16
        || rand.len() < 16
        || autn.len() < 16
        || rand.len() > 4096
        || autn.len() > 4096
    {
        return Err(DigestError::InvalidInput);
    }
    let mut nonce_bytes = Zeroizing::new(Vec::with_capacity(32));
    nonce_bytes.extend_from_slice(&rand[..16]);
    nonce_bytes.extend_from_slice(&autn[..16]);
    let nonce = Zeroizing::new(STANDARD.encode(&nonce_bytes));
    let uri = Zeroizing::new(format!("sip:{domain}"));
    let result = compute_aka_digest(
        &AkaDigestInput {
            algorithm: "AKAv1-MD5",
            username,
            realm,
            nonce: &nonce,
            method: "REGISTER",
            uri: &uri,
            qop: DigestQop::None,
        },
        AkaPassword::VerifiedEapRes(res),
    )?;

    let mut value = Zeroizing::new(String::from("Digest "));
    quoted_compact(&mut value, "username", username);
    quoted_compact(&mut value, "realm", realm);
    quoted_compact(&mut value, "nonce", &nonce);
    quoted_compact(&mut value, "uri", &uri);
    quoted_compact(&mut value, "response", &result.response);
    value.push_str(",algorithm=AKAv1-MD5");
    if value.len() > 8192 {
        return Err(DigestError::InvalidInput);
    }
    Ok(AuthorizationHeader::from_value("Authorization", value))
}

/// Parse one `WWW-Authenticate` or `Proxy-Authenticate` Digest header value.
/// Duplicate and malformed parameters are rejected rather than guessed.
pub fn parse_digest_challenge(value: &str) -> Option<DigestChallenge> {
    let parameters = crate::digest_parameters::parse(value)?;
    let get = |name: &str| {
        parameters
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    let qop = get("qop")
        .map(|value| value.split(',').map(str::trim).map(str::to_owned).collect())
        .unwrap_or_default();
    Some(DigestChallenge {
        realm: get("realm").unwrap_or_default().to_owned(),
        domain: get("domain")
            .map(str::split_whitespace)
            .map(|values| values.map(str::to_owned).collect())
            .unwrap_or_default(),
        nonce: get("nonce").unwrap_or_default().to_owned(),
        opaque: get("opaque").unwrap_or_default().to_owned(),
        stale: get("stale").is_some_and(|value| value.eq_ignore_ascii_case("true")),
        algorithm: get("algorithm").unwrap_or_default().to_owned(),
        qop,
        charset: get("charset").unwrap_or_default().to_owned(),
        userhash: get("userhash").is_some_and(|value| value.eq_ignore_ascii_case("true")),
    })
}
pub struct AuthorizationRequest<'a> {
    pub username: &'a str,
    pub method: &'a str,
    pub uri: &'a str,
    pub body: &'a [u8],
    pub cnonce: &'a str,
    pub nonce_count: u32,
}

pub struct AuthorizationHeader {
    name: &'static str,
    value: Zeroizing<String>,
}

impl AuthorizationHeader {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    /// Mark a successful AKA credential as bound to an installed IMS IPsec
    /// transport. This is intentionally explicit and consuming: callers must
    /// not advertise protection until the security-install ordering barrier
    /// has completed.
    pub(crate) fn with_integrity_protected(mut self) -> Result<Self, DigestError> {
        let parameters =
            crate::digest_parameters::parse(&self.value).ok_or(DigestError::InvalidInput)?;
        if parameters
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("integrity-protected"))
        {
            return Err(DigestError::InvalidInput);
        }
        const SUFFIX: &str = ", integrity-protected=yes";
        if self.value.len().saturating_add(SUFFIX.len()) > 8192 {
            return Err(DigestError::InvalidInput);
        }
        self.value.push_str(SUFFIX);
        Ok(self)
    }

    #[cfg(test)]
    pub(crate) fn from_parts(name: &'static str, value: impl Into<String>) -> Self {
        Self {
            name,
            value: Zeroizing::new(value.into()),
        }
    }

    pub(crate) fn from_value(name: &'static str, value: Zeroizing<String>) -> Self {
        Self { name, value }
    }
}

/// Bind authentication to the response's protection space. Do not choose an
/// arbitrary realm when more than one supported AKA challenge is present.
pub fn authorize_response(
    response: &crate::SipResponse,
    request: &AuthorizationRequest<'_>,
    password: AkaPassword<'_>,
) -> Result<AuthorizationHeader, DigestError> {
    let (name, challenge) = select_challenge(response)?;
    let value = authorize(challenge, request, password)?;
    Ok(AuthorizationHeader { name, value })
}

pub(crate) fn select_challenge(
    response: &crate::SipResponse,
) -> Result<(&'static str, &str), DigestError> {
    let (source, name) = match response.status {
        401 => ("WWW-Authenticate", "Authorization"),
        407 => ("Proxy-Authenticate", "Proxy-Authorization"),
        _ => return Err(DigestError::InvalidInput),
    };
    let mut selected = None;
    for challenge in response.headers(source) {
        if !challenge
            .split_ascii_whitespace()
            .next()
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("Digest"))
        {
            continue;
        }
        let parameters =
            crate::digest_parameters::parse(challenge).ok_or(DigestError::InvalidInput)?;
        let supported = parameters
            .iter()
            .any(|(key, value)| key == "algorithm" && value.eq_ignore_ascii_case("AKAv1-MD5"));
        if supported && selected.replace(challenge).is_some() {
            return Err(DigestError::InvalidInput);
        }
    }
    Ok((name, selected.ok_or(DigestError::UnsupportedAlgorithm)?))
}

fn quoted(out: &mut String, name: &str, value: &str) {
    if out.len() > "Digest ".len() {
        out.push_str(", ");
    }
    out.push_str(name);
    out.push_str("=\"");
    for c in value.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
}

fn quoted_compact(out: &mut String, name: &str, value: &str) {
    if out.len() > "Digest ".len() {
        out.push(',');
    }
    out.push_str(name);
    out.push_str("=\"");
    for c in value.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
}

/// Returns the header VALUE only, for the matching Authorization or
/// Proxy-Authorization header. No algorithm downgrade is permitted.
pub fn authorize(
    challenge: &str,
    request: &AuthorizationRequest<'_>,
    password: AkaPassword<'_>,
) -> Result<Zeroizing<String>, DigestError> {
    let parameters = crate::digest_parameters::parse(challenge).ok_or(DigestError::InvalidInput)?;
    let get = |name: &str| {
        parameters
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    let algorithm = get("algorithm").ok_or(DigestError::UnsupportedAlgorithm)?;
    if !algorithm.eq_ignore_ascii_case("AKAv1-MD5") {
        return Err(DigestError::UnsupportedAlgorithm);
    }
    if get("userhash").is_some_and(|v| !v.eq_ignore_ascii_case("false")) {
        return Err(DigestError::InvalidInput);
    }
    if get("charset").is_some_and(|v| !v.eq_ignore_ascii_case("UTF-8")) {
        return Err(DigestError::InvalidInput);
    }
    let realm = get("realm").ok_or(DigestError::InvalidInput)?;
    let nonce = get("nonce").ok_or(DigestError::InvalidNonce)?;
    let selected_qop = match get("qop") {
        None => None,
        Some(value) => {
            let offered: Vec<_> = value.split(',').map(str::trim).collect();
            if offered.contains(&"auth") {
                Some("auth")
            } else if offered.contains(&"auth-int") {
                Some("auth-int")
            } else {
                return Err(DigestError::InvalidQop);
            }
        }
    };
    let qop = match selected_qop {
        Some("auth") => DigestQop::Auth {
            cnonce: request.cnonce,
            count: request.nonce_count,
        },
        Some(_) => DigestQop::AuthInt {
            cnonce: request.cnonce,
            count: request.nonce_count,
            body: request.body,
        },
        None => DigestQop::None,
    };
    let result = compute_aka_digest(
        &AkaDigestInput {
            algorithm,
            username: request.username,
            realm,
            nonce,
            method: request.method,
            uri: request.uri,
            qop,
        },
        password,
    )?;
    let opaque = get("opaque").unwrap_or("");
    if opaque.bytes().any(|b| b.is_ascii_control()) {
        return Err(DigestError::InvalidInput);
    }
    let mut header = Zeroizing::new(String::from("Digest "));
    quoted(&mut header, "username", request.username);
    quoted(&mut header, "realm", realm);
    quoted(&mut header, "nonce", nonce);
    quoted(&mut header, "uri", request.uri);
    header.push_str(", algorithm=");
    header.push_str(algorithm);
    if selected_qop.is_some() {
        quoted(&mut header, "cnonce", request.cnonce);
    }
    if !opaque.is_empty() {
        quoted(&mut header, "opaque", opaque);
    }
    if let Some(qop) = selected_qop {
        header.push_str(&format!(", qop={qop}, nc={:08x}", request.nonce_count));
    }
    quoted(&mut header, "response", &result.response);
    if let Some(auts) = result.auts {
        quoted(&mut header, "auts", &auts);
    }
    if header.len() > 8192 {
        return Err(DigestError::InvalidInput);
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eap_direct_initial_authorization_matches_raw_octet_reference() {
        let rand: Vec<u8> = (0..16).collect();
        let autn: Vec<u8> = (16..32).collect();
        let header = build_eap_direct_initial_authorization(
            "private@ims.example.test",
            "ims.example.test",
            "ims.example.test",
            &[0, 0x80, 0xff, 1, 0x22, 0x33, 0x44, 0x55],
            &rand,
            &autn,
        )
        .unwrap();
        assert_eq!(header.name(), "Authorization");
        assert_eq!(
            header.value(),
            r#"Digest username="private@ims.example.test",realm="ims.example.test",nonce="AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",uri="sip:ims.example.test",response="b8597f4181552744727b69351857e1b5",algorithm=AKAv1-MD5"#
        );
    }

    #[test]
    fn eap_direct_initial_authorization_preserves_short_legacy_res() {
        let header =
            build_eap_direct_initial_authorization("u", "d", "r", &[0xaa], &[0; 16], &[1; 16])
                .unwrap();
        assert!(
            header
                .value()
                .starts_with("Digest username=\"u\",realm=\"r\",nonce=")
        );
    }

    #[test]
    fn eap_direct_initial_authorization_rejects_missing_or_invalid_material() {
        assert_eq!(
            build_eap_direct_initial_authorization("u", "d", "r", &[], &[0; 16], &[0; 16]).err(),
            Some(DigestError::InvalidInput)
        );
        assert!(
            build_eap_direct_initial_authorization("u", "d", "r", &[1; 8], &[0; 15], &[0; 16])
                .is_err()
        );
    }

    #[test]
    fn parses_ims_digest_challenge_with_quoted_commas_and_qop_order() {
        let challenge = parse_digest_challenge(
            r#"Digest realm="ims,home",domain="sip:a.test sip:b.test",nonce="n",opaque="o",stale=true,algorithm=AKAv1-MD5,qop="auth-int,auth",charset=UTF-8,userhash=true"#,
        )
        .unwrap();
        assert_eq!(challenge.realm, "ims,home");
        assert_eq!(challenge.domain, ["sip:a.test", "sip:b.test"]);
        assert_eq!(challenge.nonce, "n");
        assert_eq!(challenge.opaque, "o");
        assert!(challenge.stale);
        assert_eq!(challenge.algorithm, "AKAv1-MD5");
        assert_eq!(challenge.qop, ["auth-int", "auth"]);
        assert_eq!(challenge.charset, "UTF-8");
        assert!(challenge.userhash);
    }

    #[test]
    fn rejects_ambiguous_ims_digest_challenge() {
        assert!(parse_digest_challenge("Digest nonce=x,Nonce=y").is_none());
        assert!(parse_digest_challenge("Basic nonce=x").is_none());
    }

    #[test]
    fn integrity_protection_is_explicit_exact_and_single_use() {
        let header = AuthorizationHeader {
            name: "Authorization",
            value: Zeroizing::new(
                "Digest username=\"user\", realm=\"ims\", nonce=\"n\", uri=\"sip:ims\", algorithm=AKAv1-MD5, response=\"r\""
                    .to_owned(),
            ),
        }
        .with_integrity_protected()
        .unwrap();
        assert!(header.value.ends_with(", integrity-protected=yes"));
        assert_eq!(header.value.matches("integrity-protected").count(), 1);
        assert!(matches!(
            header.with_integrity_protected(),
            Err(DigestError::InvalidInput)
        ));
    }
    #[test]
    fn response_status_selects_authentication_space() {
        let request = AuthorizationRequest {
            username: "user@example.test",
            method: "REGISTER",
            uri: "sip:ims.example.test",
            body: b"",
            cnonce: "fixed",
            nonce_count: 1,
        };
        let challenge = format!(
            "Digest realm=\"ims.example.test\",nonce=\"{}\",algorithm=AKAv1-MD5",
            "01".repeat(32)
        );
        for (status, source, output, wrong) in [
            (
                401,
                "WWW-Authenticate",
                "Authorization",
                "Proxy-Authenticate",
            ),
            (
                407,
                "Proxy-Authenticate",
                "Proxy-Authorization",
                "WWW-Authenticate",
            ),
        ] {
            let parse = |headers: String| {
                crate::SipResponse::parse(format!("SIP/2.0 {status} Challenge\r\nCall-ID: reg-1\r\nCSeq: 1 REGISTER\r\n{headers}Content-Length: 0\r\n\r\n").as_bytes()).unwrap()
            };
            let correct = parse(format!("{source}: {challenge}\r\n"));
            assert_eq!(
                authorize_response(&correct, &request, AkaPassword::Res(&[1; 8]))
                    .unwrap()
                    .name,
                output
            );
            let incorrect = parse(format!("{wrong}: {challenge}\r\n"));
            assert!(!incorrect.has_aka_challenge());
            assert!(authorize_response(&incorrect, &request, AkaPassword::Res(&[1; 8])).is_err());
            let duplicate = parse(format!(
                "{source}: {challenge}\r\n{source}: {challenge}\r\n"
            ));
            assert!(authorize_response(&duplicate, &request, AkaPassword::Res(&[1; 8])).is_err());
        }
    }
    #[test]
    fn canonical_header_and_sync_match_go_digest_fixture() {
        let nonce = "01".repeat(32);
        let challenge = format!(
            "Digest realm=\"ims.example.test\",nonce=\"{nonce}\",algorithm=AKAv1-MD5,qop=\"auth-int,auth\""
        );
        let request = AuthorizationRequest {
            username: "user@example.test",
            method: "REGISTER",
            uri: "sip:ims.example.test",
            body: b"body\0data",
            cnonce: "fixed-client-nonce",
            nonce_count: 1,
        };
        let header = authorize(
            &challenge,
            &request,
            AkaPassword::Res(&[0xde, 0xad, 0, 0xbe, 0xef]),
        )
        .unwrap();
        assert_eq!(
            header.as_str(),
            format!(
                "Digest username=\"user@example.test\", realm=\"ims.example.test\", nonce=\"{nonce}\", uri=\"sip:ims.example.test\", algorithm=AKAv1-MD5, cnonce=\"fixed-client-nonce\", qop=auth, nc=00000001, response=\"ec22021f8bb98a4b59c216988c2a1fe6\""
            )
        );
        let sync = authorize(
            &challenge,
            &request,
            AkaPassword::SynchronizationFailure(&[0x5a; 14]),
        )
        .unwrap();
        assert!(sync.ends_with(
            "response=\"8a308c6d561c8a53ebba45c351d8033b\", auts=\"WlpaWlpaWlpaWlpaWlo=\""
        ));
        let escaped = authorize(
            &format!("{challenge},opaque=\"a\\\"b\\\\c\""),
            &request,
            AkaPassword::Res(&[1; 8]),
        )
        .unwrap();
        assert!(escaped.contains("opaque=\"a\\\"b\\\\c\""));
        for invalid in [
            challenge.replace("AKAv1-MD5", "MD5"),
            challenge.replace("auth-int,auth", "unsupported"),
            format!("{challenge},nonce=x"),
        ] {
            assert!(authorize(&invalid, &request, AkaPassword::Res(&[1; 8])).is_err());
        }
    }
}
