//! Uninstalled IMS security proposals. Parsing is not installation evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityOffer {
    pub algorithm: String,
    pub encryption: String,
    pub protocol: String,
    pub mode: String,
    pub preference: u16,
    pub spi_c: u32,
    pub spi_s: u32,
    pub port_c: u16,
    pub port_s: u16,
}
#[derive(Debug, PartialEq, Eq)]
pub enum OfferError {
    Malformed,
    UnsupportedMechanism,
    Incomplete,
    NoMatchingOffer,
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientSecurityMechanism<'a> {
    pub algorithm: &'a str,
    pub encryption: &'a str,
    pub protocol: Option<&'a str>,
    pub mode: Option<&'a str>,
}

/// Renders the ordered Security-Client capability list using the existing
/// Go wire spelling. This is a header builder only; it does not prove that a
/// server selected or installed any mechanism.
pub fn build_security_client_header(
    client: &[ClientSecurityMechanism<'_>],
    spi_c: u32,
    spi_s: u32,
    port_c: u16,
    port_s: u16,
) -> Result<String, OfferError> {
    if client.len() > 32 || port_c == 0 || port_s == 0 {
        return Err(OfferError::Malformed);
    }
    let mut offers = Vec::with_capacity(client.len());
    for mechanism in client {
        let algorithm = mechanism.algorithm.trim();
        let encryption = mechanism.encryption.trim();
        let protocol = mechanism.protocol.unwrap_or_default().trim();
        let mode = mechanism.mode.unwrap_or_default().trim();
        if algorithm.is_empty()
            || [algorithm, encryption, protocol, mode].iter().any(|value| {
                value
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || byte == b';' || byte == b',')
            })
        {
            return Err(OfferError::Malformed);
        }
        let encryption = if encryption.is_empty() {
            "null"
        } else {
            encryption
        };
        if protocol.is_empty() {
            offers.push(format!(
                "ipsec-3gpp; alg={algorithm}; ealg={encryption}; spi-c={spi_c}; spi-s={spi_s}; port-c={port_c}; port-s={port_s}"
            ));
        } else {
            let mode = if mode.is_empty() { "trans" } else { mode };
            offers.push(format!(
                "ipsec-3gpp; alg={algorithm}; ealg={encryption}; prot={protocol}; mod={mode}; spi-c={spi_c}; spi-s={spi_s}; port-c={port_c}; port-s={port_s}"
            ));
        }
    }
    let header = offers.join(",");
    if header.len() > 8192 {
        return Err(OfferError::TooLarge);
    }
    Ok(header)
}

/// Result of one Security-Server negotiation. `verify` deliberately retains
/// the P-CSCF's bytes (apart from surrounding OWS) rather than reserializing
/// parsed offers: deployed O2/giffgaff registrars reject reordered or expanded
/// Security-Verify values even when they are semantically equivalent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedSecurity {
    pub selected: SecurityOffer,
    verify: String,
}

impl NegotiatedSecurity {
    pub fn security_verify(&self) -> &str {
        &self.verify
    }
}

fn decimal(value: &str) -> Result<u32, OfferError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(OfferError::Malformed);
    }
    value.parse().map_err(|_| OfferError::Malformed)
}
fn quality(value: &str) -> Result<u16, OfferError> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if !matches!(whole, "0" | "1")
        || fraction.len() > 3
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(OfferError::Malformed);
    }
    let fraction = if fraction.is_empty() {
        0
    } else {
        decimal(fraction)? * 10u32.pow(3 - fraction.len() as u32)
    };
    if whole == "1" && fraction != 0 {
        return Err(OfferError::Malformed);
    }
    Ok(if whole == "1" { 1000 } else { fraction as u16 })
}

pub fn parse_security_server(value: &str) -> Result<Vec<SecurityOffer>, OfferError> {
    if value.len() > 8192 {
        return Err(OfferError::TooLarge);
    }
    if value.bytes().any(|b| b.is_ascii_control()) {
        return Err(OfferError::Malformed);
    }
    let mut offers = Vec::new();
    for (index, segment) in value.split(',').enumerate() {
        if index >= 32 {
            return Err(OfferError::TooLarge);
        }
        let mut parts = segment.trim().split(';');
        let mechanism = parts.next().unwrap_or("");
        if !mechanism.eq_ignore_ascii_case("ipsec-3gpp") {
            // Security-Server may advertise several mechanism families. This
            // crate implements only ipsec-3gpp; unsupported entries remain in
            // the byte-exact Security-Verify echo but are not candidates.
            continue;
        }
        let mut offer = SecurityOffer {
            algorithm: String::new(),
            // TS 33.203 permits an omitted ealg parameter to advertise the
            // integrity-only (null encryption) variant. Deployed O2 and
            // giffgaff Security-Server lists use that spelling for part of
            // their offer set, so absence is a value rather than an
            // incomplete header.
            encryption: "null".into(),
            protocol: "esp".into(),
            mode: "trans".into(),
            preference: 0,
            spi_c: 0,
            spi_s: 0,
            port_c: 0,
            port_s: 0,
        };
        let mut seen = Vec::new();
        for parameter in parts {
            let (name, value) = parameter
                .trim()
                .split_once('=')
                .ok_or(OfferError::Malformed)?;
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            if name.is_empty()
                || !name.bytes().all(crate::is_header_name_byte)
                || value.is_empty()
                || !value.bytes().all(crate::is_header_name_byte)
                || seen.contains(&name)
            {
                return Err(OfferError::Malformed);
            }
            seen.push(name.clone());
            match name.as_str() {
                "alg" => offer.algorithm = value.to_ascii_lowercase(),
                "ealg" => offer.encryption = value.to_ascii_lowercase(),
                "prot" => offer.protocol = value.to_ascii_lowercase(),
                "mod" | "mode" => {
                    if seen.contains(&if name == "mod" {
                        "mode".into()
                    } else {
                        "mod".into()
                    }) {
                        return Err(OfferError::Malformed);
                    }
                    offer.mode = value.to_ascii_lowercase();
                }
                "q" => offer.preference = quality(value)?,
                "spi-c" => {
                    offer.spi_c = decimal(value)?;
                    if offer.spi_c == 0 {
                        return Err(OfferError::Malformed);
                    }
                }
                "spi-s" => {
                    offer.spi_s = decimal(value)?;
                    if offer.spi_s == 0 {
                        return Err(OfferError::Malformed);
                    }
                }
                "port-c" | "port-s" => {
                    let port = u16::try_from(decimal(value)?).map_err(|_| OfferError::Malformed)?;
                    if port == 0 {
                        return Err(OfferError::Malformed);
                    }
                    if name == "port-c" {
                        offer.port_c = port;
                    } else {
                        offer.port_s = port;
                    }
                }
                _ => {}
            }
        }
        if offer.algorithm.is_empty() {
            return Err(OfferError::Incomplete);
        }
        offers.push(offer);
    }
    // Some deployed headers place shared endpoints only on the final offer.
    // Fill absent fields only; never overwrite explicitly advertised endpoints.
    let shared = offers
        .last()
        .ok_or(OfferError::UnsupportedMechanism)?
        .clone();
    for offer in &mut offers {
        if offer.spi_c == 0 {
            offer.spi_c = shared.spi_c;
        }
        if offer.spi_s == 0 {
            offer.spi_s = shared.spi_s;
        }
        if offer.port_c == 0 {
            offer.port_c = shared.port_c;
        }
        if offer.port_s == 0 {
            offer.port_s = shared.port_s;
        }
        if offer.spi_c == 0 || offer.spi_s == 0 || offer.port_c == 0 || offer.port_s == 0 {
            return Err(OfferError::Incomplete);
        }
    }
    Ok(offers)
}

/// Selects the first highest-q compatible offer, matching the production Go
/// behavior. In non-strict mode the first highest-q complete server offer is
/// returned only when none of the configured client mechanisms match.
pub fn select_security_server_offer<'a>(
    offers: &'a [SecurityOffer],
    client: &[ClientSecurityMechanism<'_>],
    strict: bool,
) -> Result<&'a SecurityOffer, OfferError> {
    let complete = |offer: &SecurityOffer| {
        offer.spi_c != 0 && offer.spi_s != 0 && offer.port_c != 0 && offer.port_s != 0
    };
    let matches_client = |offer: &SecurityOffer| {
        client.iter().any(|mechanism| {
            offer
                .algorithm
                .eq_ignore_ascii_case(mechanism.algorithm.trim())
                && offer.encryption.eq_ignore_ascii_case(
                    if mechanism.encryption.trim().is_empty() {
                        "null"
                    } else {
                        mechanism.encryption.trim()
                    },
                )
                && mechanism.protocol.is_none_or(|protocol| {
                    protocol.trim().is_empty()
                        || offer.protocol.eq_ignore_ascii_case(protocol.trim())
                })
                && mechanism.mode.is_none_or(|mode| {
                    mode.trim().is_empty() || offer.mode.eq_ignore_ascii_case(mode.trim())
                })
        })
    };
    let (mut best_match, mut best_any): (Option<&SecurityOffer>, Option<&SecurityOffer>) =
        (None, None);
    for offer in offers.iter().filter(|offer| complete(offer)) {
        if best_any.is_none_or(|best| offer.preference > best.preference) {
            best_any = Some(offer);
        }
        if matches_client(offer) && best_match.is_none_or(|best| offer.preference > best.preference)
        {
            best_match = Some(offer);
        }
    }
    if let Some(selected) = best_match {
        return Ok(selected);
    }
    if strict {
        return Err(OfferError::NoMatchingOffer);
    }
    best_any.ok_or(OfferError::Incomplete)
}

pub fn negotiate_security_server(
    value: &str,
    client: &[ClientSecurityMechanism<'_>],
    strict: bool,
) -> Result<NegotiatedSecurity, OfferError> {
    let verify = value.trim();
    let offers = parse_security_server(verify)?;
    let selected = select_security_server_offer(&offers, client, strict)?.clone();
    Ok(NegotiatedSecurity {
        selected,
        verify: verify.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_client_builder_preserves_go_order_and_spacing() {
        let header = build_security_client_header(
            &[
                ClientSecurityMechanism {
                    algorithm: "hmac-sha-1-96",
                    encryption: "aes-cbc",
                    protocol: Some("esp"),
                    mode: Some("trans"),
                },
                ClientSecurityMechanism {
                    algorithm: "hmac-md5-96",
                    encryption: "",
                    protocol: None,
                    mode: None,
                },
            ],
            111,
            222,
            5064,
            5063,
        )
        .unwrap();
        assert_eq!(
            header,
            "ipsec-3gpp; alg=hmac-sha-1-96; ealg=aes-cbc; prot=esp; mod=trans; spi-c=111; spi-s=222; port-c=5064; port-s=5063,ipsec-3gpp; alg=hmac-md5-96; ealg=null; spi-c=111; spi-s=222; port-c=5064; port-s=5063"
        );
    }
    #[test]
    fn reads_go_carrier_fixture_and_does_not_overwrite_explicit_endpoints() {
        let fixture = "ipsec-3gpp;q=0.98;alg=hmac-sha-1-96;mod=trans;ealg=aes-cbc;spi-c=111060141;spi-s=117229022;port-c=6050;port-s=6060";
        let parsed = parse_security_server(fixture).unwrap();
        assert_eq!(
            (parsed[0].preference, parsed[0].spi_c, parsed[0].port_s),
            (980, 111060141, 6060)
        );
        let joined = format!("ipsec-3gpp;alg=hmac-md5-96;ealg=null;spi-c=42,{fixture}");
        let parsed = parse_security_server(&joined).unwrap();
        assert_eq!(parsed[0].spi_c, 42);
        assert_eq!(parsed[0].spi_s, 117229022);
        for invalid in [
            fixture.replace("q=0.98", "q=NaN"),
            fixture.replace("port-c=6050", "port-c=65536"),
            format!("{fixture};spi-c=5"),
            fixture.replace("ipsec-3gpp", "ipsec-3gpp-extra"),
        ] {
            assert!(parse_security_server(&invalid).is_err());
        }
    }

    #[test]
    fn accepts_o2_giffgaff_six_offer_header_and_normalizes_missing_ealg() {
        let fixture = concat!(
            "ipsec-3gpp;q=0.88;alg=hmac-md5-96;mod=trans,",
            "ipsec-3gpp;q=0.9;alg=hmac-md5-96;mod=trans;ealg=des-ede3-cbc,",
            "ipsec-3gpp;q=0.92;alg=hmac-md5-96;mod=trans;ealg=aes-cbc,",
            "ipsec-3gpp;q=0.94;alg=hmac-sha-1-96;mod=trans,",
            "ipsec-3gpp;q=0.96;alg=hmac-sha-1-96;mod=trans;ealg=des-ede3-cbc,",
            "ipsec-3gpp;q=0.98;alg=hmac-sha-1-96;mod=trans;ealg=aes-cbc;",
            "spi-c=111060141;spi-s=117229022;port-c=6050;port-s=6060"
        );
        let offers = parse_security_server(fixture).unwrap();
        assert_eq!(offers.len(), 6);
        assert_eq!(offers[0].encryption, "null");
        assert_eq!(offers[3].encryption, "null");
        assert_eq!(offers[5].encryption, "aes-cbc");
        assert_eq!(offers[5].preference, 980);
        assert!(offers.iter().all(|offer| {
            offer.spi_c == 111060141
                && offer.spi_s == 117229022
                && offer.port_c == 6050
                && offer.port_s == 6060
        }));

        let negotiated = negotiate_security_server(
            fixture,
            &[ClientSecurityMechanism {
                algorithm: "hmac-sha-1-96",
                encryption: "aes-cbc",
                protocol: Some("esp"),
                mode: Some("trans"),
            }],
            true,
        )
        .unwrap();
        assert_eq!(negotiated.selected.preference, 980);
        assert_eq!(negotiated.selected.algorithm, "hmac-sha-1-96");
        assert_eq!(negotiated.selected.encryption, "aes-cbc");
        assert_eq!(negotiated.security_verify(), fixture);
        // The exact echo keeps null-ealg omissions and shared endpoints only
        // on the final textual item.
        assert!(
            !negotiated
                .security_verify()
                .split(',')
                .next()
                .unwrap()
                .contains("ealg")
        );
        assert_eq!(negotiated.security_verify().matches("spi-c=").count(), 1);

        let unsupported = [ClientSecurityMechanism {
            algorithm: "hmac-sha-256-128",
            encryption: "aes-cbc",
            protocol: Some("esp"),
            mode: Some("trans"),
        }];
        assert_eq!(
            negotiate_security_server(fixture, &unsupported, true),
            Err(OfferError::NoMatchingOffer)
        );
        assert_eq!(
            negotiate_security_server(fixture, &unsupported, false)
                .unwrap()
                .selected
                .preference,
            980
        );

        let wrong_mode = [ClientSecurityMechanism {
            algorithm: "hmac-sha-1-96",
            encryption: "aes-cbc",
            protocol: Some("esp"),
            mode: Some("tun"),
        }];
        assert_eq!(
            negotiate_security_server(fixture, &wrong_mode, true),
            Err(OfferError::NoMatchingOffer)
        );

        let mixed = format!("tls;q=1,{fixture}");
        let mixed_negotiated = negotiate_security_server(
            &mixed,
            &[ClientSecurityMechanism {
                algorithm: "hmac-sha-1-96",
                encryption: "aes-cbc",
                protocol: None,
                mode: None,
            }],
            true,
        )
        .unwrap();
        assert_eq!(mixed_negotiated.selected.preference, 980);
        assert_eq!(mixed_negotiated.security_verify(), mixed);
        assert_eq!(
            parse_security_server("tls;q=1"),
            Err(OfferError::UnsupportedMechanism)
        );
    }
}
