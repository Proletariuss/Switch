//! Strict reader-only carrier override decoding. Loading never performs DNS or
//! network I/O and does not affect an already-created immutable session.

use crate::carrier::NativeTunnelOverrides;
use http::Uri;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Debug, Deserialize, Default, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CarrierOverride {
    pub id: Option<String>,
    pub custom_epdg: Option<String>,
    pub epdg_port: Option<u16>,
    pub apn: Option<String>,
    pub ip_stack: Option<String>,
    pub dns_servers: Option<Vec<String>>,
    pub nat_keepalive_seconds: Option<u64>,
    pub dpd_interval_seconds: Option<u64>,
    pub reauth_interval_seconds: Option<u64>,
    pub algorithm_policy: Option<String>,
    pub enable_legacy_ciphers: Option<bool>,
    pub ike_proposals: Option<Vec<String>>,
    pub esp_proposals: Option<Vec<String>>,
    pub aka_challenge_mode: Option<String>,
    pub device_model: Option<String>,
    pub device_identity_enabled: Option<bool>,
    pub device_identity_imei: Option<String>,
    pub ims_domain: Option<String>,
    pub ims_realm: Option<String>,
    pub ims_phone_context: Option<String>,
    pub ims_pcscf: Option<String>,
    pub ims_transport: Option<String>,
    pub ims_identity_source: Option<String>,
    pub ims_local_port: Option<u16>,
    pub ims_tcp_keepalive_seconds: Option<u64>,
    pub ims_options_ping_interval_seconds: Option<u64>,
    pub ims_register_template: Option<ImsRegisterTemplateOverride>,
    pub e911: Option<E911Override>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ImsRegisterTemplateOverride {
    pub id: Option<String>,
    pub expires: Option<u64>,
    pub contact_mode: Option<String>,
    pub sec_agree_mode: Option<String>,
    pub supported_header: Option<String>,
    pub access_type: Option<String>,
    pub contact_param_order: Option<Vec<String>>,
    pub include_pani_authenticated: Option<bool>,
    pub strict_security_server_offer: Option<bool>,
    pub enable_initial_reject_fallback: Option<bool>,
    pub security_client_mechanisms: Option<Vec<SecurityClientMechanism>>,
    pub register_policy: Option<ImsRegisterPolicyOverride>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SecurityClientMechanism {
    pub alg: Option<String>,
    pub ealg: Option<String>,
    pub prot: Option<String>,
    pub mode: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ImsRegisterPolicyOverride {
    pub temporary_status_codes: Option<Vec<u16>>,
    pub forbidden_status_codes: Option<Vec<u16>>,
    pub initial_reject_fallback_status_codes: Option<Vec<u16>>,
    pub temporary_retry_seconds: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct E911Override {
    pub enabled: Option<bool>,
    pub provider: Option<String>,
    pub entitlement_url: Option<String>,
    pub websheet_host_policy: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideFile {
    carrier_overrides: BTreeMap<String, CarrierOverride>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OverrideSet {
    entries: BTreeMap<String, CarrierOverride>,
}

impl OverrideSet {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 256 * 1024 {
            return Err("carrier overrides file exceeds 256 KiB".into());
        }
        let document: OverrideFile = serde_yaml::from_slice(bytes)
            .map_err(|error| format!("invalid carrier overrides YAML: {error}"))?;
        let mut entries = BTreeMap::new();
        for (source, mut value) in document.carrier_overrides {
            let key = canonical_plmn_key(&source)?;
            if entries.contains_key(&key) {
                return Err(format!("duplicate normalized PLMN {key}"));
            }
            value.normalize_and_validate()?;
            entries.insert(key, value);
        }
        Ok(Self { entries })
    }

    pub fn get(&self, mcc: &str, mnc: &str) -> Option<&CarrierOverride> {
        let key = canonical_plmn(mcc, mnc).ok()?;
        self.entries.get(&key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl CarrierOverride {
    pub fn tunnel_overrides(&self) -> NativeTunnelOverrides {
        NativeTunnelOverrides {
            epdg: self.custom_epdg.clone(),
            epdg_port: self.epdg_port,
            apn: self.apn.clone(),
            ip_stack: self.ip_stack.clone(),
            ike_proposals: self.ike_proposals.clone(),
            esp_proposals: self.esp_proposals.clone(),
            aka_challenge_mode: self.aka_challenge_mode.clone(),
        }
    }

    fn normalize_and_validate(&mut self) -> Result<(), String> {
        trim(&mut self.id);
        trim(&mut self.custom_epdg);
        trim(&mut self.apn);
        trim(&mut self.ip_stack);
        trim(&mut self.algorithm_policy);
        trim(&mut self.aka_challenge_mode);
        trim(&mut self.ims_transport);
        trim(&mut self.ims_identity_source);
        trim(&mut self.device_identity_imei);
        trim(&mut self.device_model);
        trim(&mut self.ims_domain);
        trim(&mut self.ims_realm);
        trim(&mut self.ims_phone_context);
        trim(&mut self.ims_pcscf);
        if self.id.as_deref() == Some("") {
            return Err("override id must not be empty".into());
        }
        if let Some(epdg) = &self.custom_epdg
            && !epdg.is_empty()
        {
            validate_host(epdg).map_err(|error| format!("custom_epdg: {error}"))?;
        }
        enum_value(&mut self.ip_stack, &["ipv4", "ipv6", "ipv4v6"], "ip_stack")?;
        enum_value(
            &mut self.algorithm_policy,
            &["strict", "balanced", "legacy_prefer"],
            "algorithm_policy",
        )?;
        if self.algorithm_policy.as_deref() == Some("legacy_prefer")
            && self.enable_legacy_ciphers != Some(true)
        {
            return Err("legacy_prefer requires enable_legacy_ciphers=true".into());
        }
        enum_value(
            &mut self.aka_challenge_mode,
            &["minimal", "omit", "checkcode", "recompute"],
            "aka_challenge_mode",
        )?;
        enum_value(
            &mut self.ims_transport,
            &["auto", "tcp", "udp"],
            "ims_transport",
        )?;
        enum_value(
            &mut self.ims_identity_source,
            &["derived", "isim", "auto"],
            "ims_identity_source",
        )?;
        positive(self.nat_keepalive_seconds, "nat_keepalive_seconds")?;
        positive(self.dpd_interval_seconds, "dpd_interval_seconds")?;
        if self.epdg_port == Some(0) {
            return Err("epdg_port must be between 1 and 65535".into());
        }
        if self.ims_local_port == Some(0) {
            return Err("ims_local_port must be between 1 and 65535".into());
        }
        normalize_tokens(&mut self.ike_proposals, "ike_proposals")?;
        normalize_tokens(&mut self.esp_proposals, "esp_proposals")?;
        if let Some(servers) = &mut self.dns_servers {
            for server in servers {
                *server = normalize_dns_server(server)?;
            }
        }
        if let Some(imei) = &self.device_identity_imei
            && !valid_imei_luhn(imei)
        {
            return Err("device_identity_imei must be a valid 15-digit IMEI".into());
        }
        if let Some(template) = &mut self.ims_register_template {
            template.normalize_and_validate()?;
        }
        if let Some(e911) = &mut self.e911 {
            e911.normalize_and_validate()?;
        }
        // NativeTunnelConfig performs the final hostname/APN/proposal checks
        // before any I/O, preserving one validation boundary for all sources.
        Ok(())
    }
}

impl ImsRegisterTemplateOverride {
    fn normalize_and_validate(&mut self) -> Result<(), String> {
        trim(&mut self.id);
        trim(&mut self.contact_mode);
        trim(&mut self.sec_agree_mode);
        trim(&mut self.supported_header);
        trim(&mut self.access_type);
        if self.id.as_deref() == Some("") {
            return Err("ims_register_template.id must not be empty".into());
        }
        positive(self.expires, "ims_register_template.expires")?;
        enum_value(
            &mut self.contact_mode,
            &["legacy", "android_default", "gsma"],
            "contact_mode",
        )?;
        enum_value(
            &mut self.sec_agree_mode,
            &["auto", "required", "disabled"],
            "sec_agree_mode",
        )?;
        if let Some(order) = &mut self.contact_param_order {
            const ALLOWED: &[&str] = &[
                "access_type",
                "sip_instance",
                "audio",
                "smsip",
                "icsi_ref",
                "mid_call",
                "srvcc_alerting",
                "ps2cs_srvcc_orig_pre_alerting",
            ];
            let mut seen = std::collections::BTreeSet::new();
            for value in order {
                *value = value.trim().to_ascii_lowercase();
                if !ALLOWED.contains(&value.as_str()) || !seen.insert(value.clone()) {
                    return Err(
                        "contact_param_order contains an unsupported or duplicate value".into(),
                    );
                }
            }
        }
        if let Some(mechanisms) = &mut self.security_client_mechanisms {
            for mechanism in mechanisms {
                for (name, value) in [
                    ("alg", &mut mechanism.alg),
                    ("ealg", &mut mechanism.ealg),
                    ("prot", &mut mechanism.prot),
                    ("mode", &mut mechanism.mode),
                ] {
                    trim(value);
                    if matches!(name, "alg" | "ealg") && value.as_deref().unwrap_or("").is_empty() {
                        return Err(format!("security_client_mechanisms.{name} is required"));
                    }
                    if let Some(value) = value {
                        *value = value.to_ascii_lowercase();
                        if !is_token(value) {
                            return Err(format!("security_client_mechanisms.{name} is invalid"));
                        }
                    }
                }
            }
        }
        if let Some(policy) = &self.register_policy {
            validate_statuses(&policy.temporary_status_codes)?;
            validate_statuses(&policy.forbidden_status_codes)?;
            validate_statuses(&policy.initial_reject_fallback_status_codes)?;
        }
        Ok(())
    }
}

impl E911Override {
    fn normalize_and_validate(&mut self) -> Result<(), String> {
        trim(&mut self.provider);
        trim(&mut self.entitlement_url);
        trim(&mut self.websheet_host_policy);
        enum_value(
            &mut self.websheet_host_policy,
            &["public_https"],
            "websheet_host_policy",
        )?;
        if let Some(raw) = &self.entitlement_url {
            let parsed: Uri = raw.parse().map_err(|_| "e911 entitlement_url is invalid")?;
            let authority = parsed
                .authority()
                .ok_or("e911 entitlement_url host is missing")?;
            if parsed.scheme_str() != Some("https")
                || authority.as_str().contains('@')
                || raw.contains('#')
            {
                return Err("e911 entitlement_url must be absolute public HTTPS".into());
            }
            let host = parsed
                .host()
                .ok_or("e911 entitlement_url host is missing")?;
            let normalized = host
                .trim_matches(['[', ']'])
                .trim_end_matches('.')
                .to_ascii_lowercase();
            if normalized == "localhost" || normalized.ends_with(".localhost") {
                return Err("e911 entitlement_url host is not public".into());
            }
            if let Ok(address) = normalized.parse::<IpAddr>()
                && !public_ip(address)
            {
                return Err("e911 entitlement_url host is not public".into());
            }
        }
        if self.enabled == Some(true)
            && (self.provider.as_deref().unwrap_or("").is_empty()
                || self.entitlement_url.as_deref().unwrap_or("").is_empty()
                || self.websheet_host_policy.as_deref() != Some("public_https"))
        {
            return Err("enabled e911 requires provider, entitlement_url and public_https".into());
        }
        Ok(())
    }
}

fn valid_imei_luhn(value: &str) -> bool {
    if value.len() != 15 || !value.bytes().all(|digit| digit.is_ascii_digit()) {
        return false;
    }
    value
        .bytes()
        .enumerate()
        .map(|(index, byte)| {
            let mut digit = u32::from(byte - b'0');
            if index % 2 == 1 {
                digit *= 2;
                if digit > 9 {
                    digit -= 9;
                }
            }
            digit
        })
        .sum::<u32>()
        % 10
        == 0
}

fn trim(value: &mut Option<String>) {
    if let Some(value) = value {
        *value = value.trim().to_owned();
    }
}

fn enum_value(value: &mut Option<String>, allowed: &[&str], name: &str) -> Result<(), String> {
    let Some(value) = value else { return Ok(()) };
    *value = value.to_ascii_lowercase();
    if allowed.contains(&value.as_str()) {
        Ok(())
    } else {
        Err(format!("{name} has unsupported value"))
    }
}

fn normalize_tokens(values: &mut Option<Vec<String>>, name: &str) -> Result<(), String> {
    let Some(values) = values else { return Ok(()) };
    for value in values {
        *value = value.trim().to_ascii_lowercase();
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(format!("{name} contains an invalid token"));
        }
    }
    Ok(())
}

fn positive(value: Option<u64>, name: &str) -> Result<(), String> {
    if value == Some(0) {
        Err(format!("{name} must be greater than zero"))
    } else {
        Ok(())
    }
}

fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_statuses(values: &Option<Vec<u16>>) -> Result<(), String> {
    let Some(values) = values else { return Ok(()) };
    let mut seen = std::collections::BTreeSet::new();
    for status in values {
        if !(100..=699).contains(status) {
            return Err("register_policy status code must be between 100 and 699".into());
        }
        if !seen.insert(*status) {
            return Err(format!(
                "register_policy contains duplicate status code {status}"
            ));
        }
    }
    Ok(())
}

fn normalize_dns_server(raw: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err("DNS server address must not be empty".into());
    }
    if let Ok(address) = value.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Ok(SocketAddr::new(address, 53).to_string());
    }
    if let Ok(address) = value.parse::<SocketAddr>() {
        if address.port() == 0 {
            return Err("DNS server port must be between 1 and 65535".into());
        }
        return Ok(address.to_string());
    }
    if value.contains(':') {
        let (host, port) = value
            .rsplit_once(':')
            .ok_or("DNS server has an invalid or missing port")?;
        if host.contains(':') {
            return Err("DNS server has an invalid or missing port".into());
        }
        validate_host(host)?;
        let port = port
            .parse::<u16>()
            .map_err(|_| "DNS server port must be between 1 and 65535")?;
        if port == 0 {
            return Err("DNS server port must be between 1 and 65535".into());
        }
        return Ok(format!("{}:{port}", host.to_ascii_lowercase()));
    }
    validate_host(value)?;
    Ok(format!("{}:53", value.to_ascii_lowercase()))
}

fn validate_host(raw: &str) -> Result<(), String> {
    let host = raw.trim();
    if host.is_empty() {
        return Err("host must not be empty".into());
    }
    if host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    if host.len() > 253 || host.starts_with('.') || host.ends_with('.') {
        return Err("host is not a valid IP address or DNS name".into());
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("host is not a valid IP address or DNS name".into());
        }
    }
    Ok(())
}

fn public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            !address.is_loopback()
                && !address.is_private()
                && !address.is_link_local()
                && !address.is_unspecified()
                && !address.is_multicast()
        }
        IpAddr::V6(address) => {
            !address.is_loopback()
                && !address.is_unique_local()
                && !address.is_unicast_link_local()
                && !address.is_unspecified()
                && !address.is_multicast()
        }
    }
}

fn canonical_plmn_key(value: &str) -> Result<String, String> {
    let (mcc, mnc) = value
        .trim()
        .split_once('/')
        .ok_or("PLMN key must use MCC/MNC")?;
    if mnc.contains('/') {
        return Err("PLMN key must use MCC/MNC".into());
    }
    canonical_plmn(mcc, mnc)
}

fn canonical_plmn(mcc: &str, mnc: &str) -> Result<String, String> {
    let (mcc, mnc) = (mcc.trim(), mnc.trim());
    if mcc.len() != 3
        || !(2..=3).contains(&mnc.len())
        || !mcc
            .bytes()
            .chain(mnc.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return Err("PLMN requires 3-digit MCC and 2/3-digit MNC".into());
    }
    let normalized = mnc.trim_start_matches('0');
    Ok(format!(
        "{mcc}/{}",
        if normalized.is_empty() {
            "0"
        } else {
            normalized
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct SharedCase {
        name: String,
        accepted: bool,
        #[serde(default)]
        normalized_dns: Option<Vec<String>>,
        yaml: String,
    }

    #[test]
    fn shared_go_rust_acceptance_cases_match() {
        let cases: Vec<SharedCase> =
            serde_json::from_str(include_str!("../../../carrier-overrides-cases.json")).unwrap();
        for case in cases {
            let result = OverrideSet::parse(case.yaml.as_bytes());
            assert_eq!(
                result.is_ok(),
                case.accepted,
                "shared case {} returned {result:?}",
                case.name
            );
            if let Some(expected) = case.normalized_dns {
                let set = result.unwrap();
                assert_eq!(
                    set.get("310", "260").unwrap().dns_servers.as_ref(),
                    Some(&expected),
                    "shared case {} normalized DNS differs",
                    case.name
                );
            }
        }
    }

    #[test]
    fn complete_go_shape_decodes_and_preserves_presence() {
        let yaml = br#"
carrier_overrides:
  "310/260":
    id: tmobile_custom
    custom_epdg: epdg.example.net
    epdg_port: 4500
    apn: ims
    ip_stack: ipv4v6
    algorithm_policy: balanced
    enable_legacy_ciphers: false
    ike_proposals: [aes256-sha256-prfsha1-modp2048]
    esp_proposals: [aes256-sha256]
    aka_challenge_mode: minimal
    ims_transport: auto
    ims_identity_source: isim
    ims_register_template:
      id: custom_reg
      expires: 600000
      contact_mode: android_default
      sec_agree_mode: auto
      security_client_mechanisms:
        - {alg: hmac-sha-1-96, ealg: aes-cbc, prot: esp, mode: trans}
      register_policy:
        temporary_status_codes: [408, 503]
        temporary_retry_seconds: 60
    e911:
      enabled: true
      provider: att_entitlement
      entitlement_url: https://example.net/
      websheet_host_policy: public_https
"#;
        let set = OverrideSet::parse(yaml).unwrap();
        assert_eq!(set.len(), 1);
        let item = set.get("310", "260").unwrap();
        assert_eq!(item.custom_epdg.as_deref(), Some("epdg.example.net"));
        assert_eq!(item.tunnel_overrides().epdg_port, Some(4500));
        assert!(item.ims_register_template.is_some());
    }

    #[test]
    fn unknown_duplicate_and_invalid_values_fail_closed() {
        for yaml in [
            "carrier_overrides:\n  '310/260': {unknown: true}\n",
            "carrier_overrides:\n  '234/033': {id: one}\n  '234/33': {id: two}\n",
            "carrier_overrides:\n  '310/260': {algorithm_policy: legacy_prefer, enable_legacy_ciphers: false}\n",
            "carrier_overrides:\n  '310/260': {ip_stack: dual}\n",
        ] {
            assert!(OverrideSet::parse(yaml.as_bytes()).is_err());
        }
    }
}
