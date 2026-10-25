//! Carrier-aware IMS Contact header formatting shared by the native IMS owner.
use std::net::IpAddr;

const DEFAULT_EXPIRY_SECONDS: i64 = 3600;
const DEFAULT_ACCESS_TYPE: &str = "IEEE-802.11";
const DEFAULT_ICSI_REF: &str = "urn:urn-7:3gpp-service.ims.icsi.mmtel";
const DEFAULT_PARAMETER_ORDER: &[&str] =
    &["access_type", "audio", "smsip", "icsi_ref", "sip_instance"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactTemplate<'a> {
    pub transport: &'a str,
    pub contact_mode: &'a str,
    pub contact_features: &'a str,
    pub contact_param_order: Option<&'a [String]>,
    pub access_type: &'a str,
    pub icsi_ref: &'a str,
    pub expires: i64,
    pub force_header_port_5060: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactBuildInput<'a> {
    pub imsi: &'a str,
    pub public_uri: &'a str,
    pub local_ip: &'a str,
    pub local_port: i64,
    pub sip_instance_urn: &'a str,
    pub register_expiry_secs: i64,
    pub sms_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactHeaderError {
    InvalidInput,
    InvalidParameter,
    TooLarge,
}

pub fn build_contact_header(
    template: &ContactTemplate<'_>,
    input: &ContactBuildInput<'_>,
) -> Result<String, ContactHeaderError> {
    if [
        template.transport,
        template.contact_mode,
        template.contact_features,
        template.access_type,
        template.icsi_ref,
        input.imsi,
        input.public_uri,
        input.local_ip,
        input.sip_instance_urn,
    ]
    .iter()
    .any(|value| value.bytes().any(|byte| byte.is_ascii_control()))
    {
        return Err(ContactHeaderError::InvalidInput);
    }
    let ip = input
        .local_ip
        .trim()
        .parse::<IpAddr>()
        .map_err(|_| ContactHeaderError::InvalidInput)?;
    let mut user = input.imsi.trim();
    if let Some((_, rest)) = input.public_uri.split_once(':') {
        if let Some((candidate, _)) = rest.split_once('@') {
            if !candidate.is_empty() {
                user = candidate;
            }
        }
    }
    let port = if template.force_header_port_5060 || input.local_port <= 0 {
        5060
    } else {
        input.local_port
    };
    if port > u16::MAX as i64 {
        return Err(ContactHeaderError::InvalidInput);
    }
    let endpoint = socket_address(ip, port as u16);
    let mode = template.contact_mode.trim().to_ascii_lowercase();
    let contact_uri = match mode.as_str() {
        "legacy" => input.public_uri.trim().to_owned(),
        "gsma" => format!("sip:{endpoint}"),
        _ => {
            let mut transport = template.transport.trim().to_ascii_lowercase();
            if transport.is_empty() || transport == "auto" {
                transport = "tcp".to_owned();
            }
            format!("sip:{user}@{endpoint};transport={transport}")
        }
    };

    let is_sms_only = input.sms_only || template.contact_features == "sms_only";
    let source_order: Vec<&str> = if template.contact_param_order.is_none() {
        DEFAULT_PARAMETER_ORDER.to_vec()
    } else {
        template
            .contact_param_order
            .unwrap_or_default()
            .iter()
            .map(String::as_str)
            .collect()
    };
    let mut parameters: Vec<&str> = Vec::with_capacity(source_order.len() + 1);
    let mut has_smsip = false;
    for parameter in source_order {
        let normalized = parameter.trim().to_ascii_lowercase();
        if is_sms_only
            && matches!(
                normalized.as_str(),
                "audio"
                    | "icsi_ref"
                    | "mid_call"
                    | "srvcc_alerting"
                    | "ps2cs_srvcc_orig_pre_alerting"
            )
        {
            continue;
        }
        if normalized == "smsip" {
            has_smsip = true;
        }
        parameters.push(parameter);
    }
    if is_sms_only && !has_smsip {
        parameters.push("smsip");
    }

    let mut output = format!("<{contact_uri}>");
    for parameter in parameters {
        match parameter.trim().to_ascii_lowercase().as_str() {
            "access_type" => {
                let access_type = if template.access_type.trim().is_empty() {
                    DEFAULT_ACCESS_TYPE
                } else {
                    template.access_type.trim()
                };
                output.push_str(";+g.3gpp.accesstype=\"");
                output.push_str(access_type);
                output.push('"');
            }
            "sip_instance" => {
                let urn = input.sip_instance_urn.trim();
                if !urn.is_empty() {
                    output.push_str(";+sip.instance=\"<");
                    output.push_str(urn);
                    output.push_str(">\"");
                }
            }
            "audio" => output.push_str(";audio"),
            "smsip" => output.push_str(";+g.3gpp.smsip"),
            "icsi_ref" => {
                let value = if template.icsi_ref.trim().is_empty() {
                    DEFAULT_ICSI_REF
                } else {
                    template.icsi_ref.trim()
                };
                output.push_str(";+g.3gpp.icsi-ref=\"");
                output.push_str(&value.replace(':', "%3A"));
                output.push('"');
            }
            "mid_call" => output.push_str(";+g.3gpp.mid-call"),
            "srvcc_alerting" => output.push_str(";+g.3gpp.srvcc-alerting"),
            "ps2cs_srvcc_orig_pre_alerting" => {
                output.push_str(";+g.3gpp.ps2cs-srvcc-orig-pre-alerting")
            }
            _ => {} // Match the legacy Go builder: unknown ordered parameters are omitted.
        }
        if output.len() > 8192 {
            return Err(ContactHeaderError::TooLarge);
        }
    }
    if mode != "gsma" {
        let expiry = if input.register_expiry_secs > 0 {
            input.register_expiry_secs
        } else if template.expires > 0 {
            template.expires
        } else {
            DEFAULT_EXPIRY_SECONDS
        };
        output.push_str(";expires=");
        output.push_str(&expiry.to_string());
    }
    if output.len() > 8192 {
        return Err(ContactHeaderError::TooLarge);
    }
    Ok(output)
}

fn socket_address(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(ip) => format!("{ip}:{port}"),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ipv4) => format!("{ipv4}:{port}"),
            None => format!("[{ip}]:{port}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> ContactTemplate<'static> {
        ContactTemplate {
            transport: "tcp",
            contact_mode: "android_default",
            contact_features: "",
            contact_param_order: None,
            access_type: "IEEE-802.11",
            icsi_ref: DEFAULT_ICSI_REF,
            expires: 3600,
            force_header_port_5060: false,
        }
    }

    fn input<'a>(ip: &'a str) -> ContactBuildInput<'a> {
        ContactBuildInput {
            imsi: "310260123456789",
            public_uri: "sip:310260123456789@ims.example.test",
            local_ip: ip,
            local_port: 5070,
            sip_instance_urn: "urn:uuid:test",
            register_expiry_secs: 0,
            sms_only: false,
        }
    }

    #[test]
    fn default_contact_matches_legacy_android_shape_and_encoding() {
        let got = build_contact_header(&template(), &input("2001:db8::20")).unwrap();
        assert_eq!(
            got,
            "<sip:310260123456789@[2001:db8::20]:5070;transport=tcp>;+g.3gpp.accesstype=\"IEEE-802.11\";audio;+g.3gpp.smsip;+g.3gpp.icsi-ref=\"urn%3Aurn-7%3A3gpp-service.ims.icsi.mmtel\";+sip.instance=\"<urn:uuid:test>\";expires=3600"
        );
    }

    #[test]
    fn sms_only_filters_voice_features_and_adds_smsip_once() {
        let mut profile = template();
        let order = vec!["audio".into(), "icsi_ref".into(), "sip_instance".into()];
        profile.contact_param_order = Some(&order);
        let mut args = input("192.0.2.1");
        args.sms_only = true;
        let got = build_contact_header(&profile, &args).unwrap();
        assert_eq!(
            got,
            "<sip:310260123456789@192.0.2.1:5070;transport=tcp>;+sip.instance=\"<urn:uuid:test>\";+g.3gpp.smsip;expires=3600"
        );
    }

    #[test]
    fn gsma_and_legacy_modes_preserve_host_and_expiry_rules() {
        let mut profile = template();
        profile.contact_mode = "gsma";
        let empty_order: Vec<String> = Vec::new();
        profile.contact_param_order = Some(&empty_order);
        assert_eq!(
            build_contact_header(&profile, &input("2001:db8::20")).unwrap(),
            "<sip:[2001:db8::20]:5070>"
        );
        profile.contact_mode = "legacy";
        profile.force_header_port_5060 = true;
        assert_eq!(
            build_contact_header(&profile, &input("192.0.2.1")).unwrap(),
            "<sip:310260123456789@ims.example.test>;expires=3600"
        );
    }

    #[test]
    fn rejects_invalid_addresses_and_header_injection() {
        assert_eq!(
            build_contact_header(&template(), &input("not-an-ip")),
            Err(ContactHeaderError::InvalidInput)
        );
        let mut profile = template();
        profile.access_type = "wifi\r\nInjected: yes";
        assert_eq!(
            build_contact_header(&profile, &input("192.0.2.1")),
            Err(ContactHeaderError::InvalidInput)
        );
    }

    #[test]
    fn ipv4_mapped_addresses_match_go_net_ip_string_behavior() {
        let got = build_contact_header(&template(), &input("::ffff:192.0.2.1")).unwrap();
        assert!(got.starts_with("<sip:310260123456789@192.0.2.1:5070;transport=tcp>"));
    }
}
