//! Complete startup policy resolved from the same carrier/template/YAML snapshot.
//! Runtime policy is separate from subscriber credentials and never performs I/O.
use crate::{
    carrier::{NativeCarrierTunnelConfig, ResolvedCarrier, SimIdentity},
    carrier_overrides::CarrierOverride,
    carrier_runtime_profile::RuntimeCarrierProfile,
};
use serde_json::Value;

mod proposals;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeTunnelRuntimeConfig {
    pub nat_keepalive_seconds: u64,
    pub dpd_interval_seconds: u64,
    pub disable_dpd: bool,
    pub reauth_interval_seconds: u64,
    /// Modem RF settling time. Reader-backed sessions must not consume it.
    pub rf_off_delay_seconds: u64,
    pub disable_switch_to_natt_port: bool,
    pub cp_in_final_auth: bool,
    pub cookie_payload_type: String,
    pub rekey_pfs: u16,
    pub ike_local_port: u16,
    pub eap_identity_source: String,
    pub dns_servers: Vec<String>,
    pub profile: RuntimeCarrierProfile,
    pub override_id: String,
}

impl ResolvedCarrier {
    pub fn native_session_tunnel_config(
        &self,
        sim: &SimIdentity,
        override_value: Option<&CarrierOverride>,
        request_epdg: &str,
        request_port: Option<u16>,
    ) -> Result<(NativeCarrierTunnelConfig, NativeTunnelRuntimeConfig), String> {
        let profile = RuntimeCarrierProfile::expand(self, sim, "")?;
        let template = profile.vowifi_template.as_ref();
        let mut overrides = override_value
            .map(CarrierOverride::tunnel_overrides)
            .unwrap_or_default();
        if overrides
            .epdg
            .as_deref()
            .is_none_or(|v| v.trim().is_empty())
        {
            overrides.epdg = profile
                .epdg_candidates
                .as_ref()
                .and_then(|v| v.first())
                .cloned();
        }
        if overrides.apn.is_none() {
            overrides.apn = profile.apn.clone();
        }
        if overrides.ip_stack.is_none() {
            overrides.ip_stack = profile.ip_stack.clone();
        }
        if overrides.ike_proposals.is_none() {
            overrides.ike_proposals = strings(template, "/ike/proposals")?;
        }
        if overrides.esp_proposals.is_none() {
            overrides.esp_proposals = strings(template, "/ike/esp_proposals")?;
        }
        if overrides.aka_challenge_mode.is_none() {
            overrides.aka_challenge_mode = string(template, "/eap/challenge_mode")?;
        }
        // Explicit algorithm policy replaces defaults only, retaining each explicit list.
        if let Some(value) = override_value
            && (value.algorithm_policy.is_some() || value.enable_legacy_ciphers.is_some())
        {
            let (ike, esp) = algorithm_defaults(value);
            if value.ike_proposals.is_none() {
                overrides.ike_proposals = Some(ike);
            }
            if value.esp_proposals.is_none() {
                overrides.esp_proposals = Some(esp);
            }
        }
        if !request_epdg.trim().is_empty() {
            overrides.epdg = Some(request_epdg.trim().trim_matches(['[', ']']).into());
        }
        if request_port.is_some() {
            overrides.epdg_port = request_port;
        }
        // This is the compatibility session policy, distinct from the strict
        // raw-template compiler. Preserve Go's exact-template eligibility and
        // generic defaults whenever no authoritative user list replaces them.
        proposals::resolve(&profile, &mut overrides);
        let mut tunnel = self.native_tunnel_config(sim, &overrides)?;
        tunnel.validate_remote_certificate = profile.validate_remote_certificate;
        // Match the complete Go session template: its resolved ISIM/EAP
        // identity also supplies IDi, ahead of the older carrier identifier.
        // set_access_identity later freezes that exact leased-card identity.
        if profile.vowifi_template.is_some() {
            tunnel.use_access_identity_for_ike();
        }
        tunnel.epdg_source = if !request_epdg.trim().is_empty() {
            "custom"
        } else if override_value
            .and_then(|v| v.custom_epdg.as_deref())
            .is_some_and(|v| !v.trim().is_empty())
        {
            "carrier-override"
        } else if profile
            .epdg_candidates
            .as_ref()
            .is_some_and(|v| !v.is_empty())
        {
            "carrier-template"
        } else {
            "3gpp-standard"
        };
        let nat_keepalive_seconds = override_value
            .and_then(|v| v.nat_keepalive_seconds)
            .unwrap_or(profile.natt_keepalive.max(1) as u64);
        let dpd_interval_seconds = override_value
            .and_then(|v| v.dpd_interval_seconds)
            .unwrap_or(profile.dpd_interval.max(1) as u64);
        let disable_dpd = profile.disable_dpd
            && override_value
                .and_then(|v| v.dpd_interval_seconds)
                .is_none();
        let reauth_interval_seconds = override_value
            .and_then(|v| v.reauth_interval_seconds)
            .filter(|v| *v > 0)
            .unwrap_or(number(template, "/ike/reauth_interval")?.unwrap_or(0));
        let first_cp = boolean(template, "/ike/cp_in_first_auth")?.unwrap_or(true);
        let final_cp = boolean(template, "/ike/cp_in_final_auth")?.unwrap_or(false);
        let cookie_payload_type =
            string(template, "/ike/cookie_payload_type")?.unwrap_or("notify".into());
        if !matches!(cookie_payload_type.as_str(), "sa" | "notify") {
            return Err("unsupported cookie payload type".into());
        }
        let rekey_pfs = u16::try_from(number(template, "/ike/rekey_pfs")?.unwrap_or(0))
            .map_err(|_| "invalid rekey PFS")?;
        if !matches!(rekey_pfs, 0 | 14) {
            return Err("unsupported rekey PFS group".into());
        }
        let ike_local_port = u16::try_from(number(template, "/ike/local_port")?.unwrap_or(0))
            .map_err(|_| "invalid IKE local port")?;
        let eap_identity_source = string(template, "/eap/identity_source")?.unwrap_or_else(|| {
            if profile.eap_identity_template.is_some() {
                "carrier-template"
            } else {
                "derived"
            }
            .into()
        });
        if !matches!(
            eap_identity_source.as_str(),
            "derived" | "isim" | "carrier-template"
        ) {
            return Err("unsupported EAP identity source".into());
        }
        Ok((
            tunnel,
            NativeTunnelRuntimeConfig {
                nat_keepalive_seconds,
                dpd_interval_seconds,
                disable_dpd,
                reauth_interval_seconds,
                rf_off_delay_seconds: number(template, "/ike/rf_off_delay")?.unwrap_or(0),
                disable_switch_to_natt_port: profile.disable_switch_to_natt_port,
                cp_in_final_auth: final_cp && !first_cp,
                cookie_payload_type,
                rekey_pfs,
                ike_local_port,
                eap_identity_source,
                dns_servers: override_value
                    .and_then(|v| v.dns_servers.clone())
                    .unwrap_or_default(),
                profile,
                override_id: override_value
                    .and_then(|v| v.id.clone())
                    .unwrap_or_default(),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{carrier::Registry, carrier_overrides::OverrideSet};
    fn sim(mcc: &str, mnc: &str) -> SimIdentity {
        SimIdentity {
            mcc: mcc.into(),
            mnc: mnc.into(),
            imsi: format!("{mcc}{mnc}1234567890"),
            iccid: "8986001234567890123".into(),
            gid1: String::new(),
            gid2: String::new(),
            spn: String::new(),
        }
    }
    #[test]
    fn three_hk_full_template_matches_production_go_effective_snapshot() {
        let sim = sim("454", "03");
        let carrier = Registry::embedded()
            .unwrap()
            .resolve(&sim)
            .unwrap()
            .unwrap();
        let (config, runtime) = carrier
            .native_session_tunnel_config(&sim, None, "", None)
            .unwrap();
        assert_eq!(config.epdg, "wlan.three.com.hk");
        assert_eq!(config.epdg_source, "carrier-template");
        assert_eq!(config.ip_stack, crate::RequestedIpStack::Ipv4);
        assert_eq!(
            config.proposals.ike,
            vec![
                "aes128-sha256-modp2048",
                "aes256-sha256-modp2048",
                "aes256-sha512-modp2048"
            ]
        );
        assert_eq!(config.proposals.esp, vec!["aes256-sha256"]);
        assert_eq!(runtime.reauth_interval_seconds, 7200);
        assert_eq!(runtime.rf_off_delay_seconds, 5);
        assert_eq!(runtime.ike_local_port, 500);
        assert_eq!(runtime.rekey_pfs, 14);
        assert!(!runtime.cp_in_final_auth);
        assert_eq!(runtime.cookie_payload_type, "sa");
        assert_eq!(runtime.eap_identity_source, "isim");
    }
    #[test]
    fn yaml_overrides_template_and_request_overrides_only_epdg_controls() {
        let sim = sim("454", "03");
        let carrier = Registry::embedded()
            .unwrap()
            .resolve(&sim)
            .unwrap()
            .unwrap();
        let set=OverrideSet::parse(b"carrier_overrides:\n  '454/03':\n    custom_epdg: yaml.example\n    epdg_port: 5555\n    apn: custom\n    ip_stack: ipv6\n    nat_keepalive_seconds: 9\n    dpd_interval_seconds: 13\n    reauth_interval_seconds: 90\n    ike_proposals: [aes128-sha256-prfsha256-modp2048]\n    esp_proposals: [aes128-sha256]\n    dns_servers: [8.8.8.8]\n").unwrap();
        let value = set.get("454", "03").unwrap();
        let (config, runtime) = carrier
            .native_session_tunnel_config(&sim, Some(value), "custom.example", Some(4444))
            .unwrap();
        assert_eq!(
            (config.epdg.as_str(), config.epdg_port, config.epdg_source),
            ("custom.example", 4444, "custom")
        );
        assert_eq!(config.apn, "custom");
        assert_eq!(config.ip_stack, crate::RequestedIpStack::Ipv6);
        assert_eq!(
            (
                runtime.nat_keepalive_seconds,
                runtime.dpd_interval_seconds,
                runtime.reauth_interval_seconds
            ),
            (9, 13, 90)
        );
        assert_eq!(runtime.dns_servers, ["8.8.8.8:53"]);
        assert!(!runtime.disable_dpd);
        let (config, _) = carrier
            .native_session_tunnel_config(&sim, Some(value), "", None)
            .unwrap();
        assert_eq!(config.epdg_source, "carrier-override");
    }
    #[test]
    fn explicit_dpd_interval_enables_carrier_disabled_probe_without_touching_nat_policy() {
        let sim = sim("234", "33");
        let carrier = Registry::embedded()
            .unwrap()
            .resolve(&sim)
            .unwrap()
            .unwrap();
        let (_, base) = carrier
            .native_session_tunnel_config(&sim, None, "", None)
            .unwrap();
        assert!(base.disable_dpd);
        let value = CarrierOverride {
            dpd_interval_seconds: Some(12),
            ..Default::default()
        };
        let (_, configured) = carrier
            .native_session_tunnel_config(&sim, Some(&value), "", None)
            .unwrap();
        assert!(!configured.disable_dpd);
        assert_eq!(configured.dpd_interval_seconds, 12);
        assert_eq!(configured.nat_keepalive_seconds, base.nat_keepalive_seconds);
    }
    #[test]
    fn access_isim_absence_is_distinct_from_partial_and_complete_snapshot() {
        assert_eq!(
            resolve_access_identity("isim", b"0imsi@nai.example", "", "", "").unwrap(),
            b"0imsi@nai.example"
        );
        assert!(
            resolve_access_identity("isim", b"0imsi@nai.example", "impi@example", "", "").is_err()
        );
        assert_eq!(
            resolve_access_identity(
                "isim",
                b"0imsi@nai.example",
                "impi@example",
                "sip:impu@example",
                "example"
            )
            .unwrap(),
            b"impi@example"
        );
        assert_eq!(
            resolve_access_identity(
                "derived",
                b"0imsi@nai.example",
                "impi@example",
                "sip:impu@example",
                "example"
            )
            .unwrap(),
            b"0imsi@nai.example"
        );
    }
    #[test]
    fn carrier_idi_is_frozen_separately_from_the_access_identity() {
        let sim = sim("001", "01");
        let carrier = ResolvedCarrier {
            id: "identity-test".into(),
            bundle: String::new(),
            selector: String::new(),
            carrier_name: None,
            configuration: serde_json::json!({"ims":{"ike":{
                "username":"0$imsi@nai.example",
                "local_identifier":"anonymous@mnc$mnc.mcc$mcc.example"
            }}}),
        };
        let (mut config, runtime) = carrier
            .native_session_tunnel_config(&sim, None, "", None)
            .unwrap();
        assert!(runtime.profile.vowifi_template.is_none());
        assert_eq!(
            config.identity(),
            format!("0{}@nai.example", sim.imsi).as_bytes()
        );
        assert_eq!(
            config.ike_local_identity(),
            Some(&b"anonymous@mnc001.mcc001.example"[..])
        );
        config
            .set_access_identity(b"impi@isim.example".to_vec())
            .unwrap();
        assert_eq!(config.identity(), b"impi@isim.example");
        assert_eq!(
            config.ike_local_identity(),
            Some(&b"anonymous@mnc001.mcc001.example"[..])
        );
    }
    #[test]
    fn complete_three_hk_template_uses_the_resolved_isim_identity_for_idi() {
        let sim = sim("454", "03");
        let carrier = Registry::embedded()
            .unwrap()
            .resolve(&sim)
            .unwrap()
            .unwrap();
        let (mut config, runtime) = carrier
            .native_session_tunnel_config(&sim, None, "", None)
            .unwrap();
        assert!(runtime.profile.vowifi_template.is_some());
        config
            .set_access_identity(b"impi@three.example".to_vec())
            .unwrap();
        assert_eq!(config.identity(), b"impi@three.example");
        assert_eq!(
            config.ike_local_identity(),
            Some(&b"impi@three.example"[..])
        );
    }
    #[test]
    fn absent_identifier_retains_eap_fallback_and_invalid_idi_fails_closed() {
        let sim = sim("001", "01");
        let mut carrier = ResolvedCarrier {
            id: "identity-test".into(),
            bundle: String::new(),
            selector: String::new(),
            carrier_name: None,
            configuration: serde_json::json!({"ims":{"ike":{"username":"nai@example"}}}),
        };
        let (mut config, _) = carrier
            .native_session_tunnel_config(&sim, None, "", None)
            .unwrap();
        assert_eq!(config.ike_local_identity(), None);
        config
            .set_access_identity(b"resolved@example".to_vec())
            .unwrap();
        assert_eq!(
            config.ike_local_identity().unwrap_or(config.identity()),
            b"resolved@example"
        );
        for value in ["bad\nidentity".to_owned(), "x".repeat(256), String::new()] {
            carrier.configuration["ims"]["ike"]["local_identifier"] = Value::String(value);
            assert!(
                carrier
                    .native_session_tunnel_config(&sim, None, "", None)
                    .is_err()
            );
        }
    }
    #[test]
    fn unsupported_udp_and_enabled_e911_are_not_silently_represented_as_tcp_or_ready() {
        let sim = sim("454", "03");
        let carrier = Registry::embedded()
            .unwrap()
            .resolve(&sim)
            .unwrap()
            .unwrap();
        let value = CarrierOverride {
            ims_transport: Some("udp".into()),
            ..Default::default()
        };
        assert!(matches!(
            carrier.native_ims_register_profile_with_home_domain(
                &sim,
                Some(&value),
                Some("ims.custom.example"),
                false
            ),
            Err(crate::ims_profile::NativeImsProfileError::UnsupportedTransport)
        ));
        let profile = carrier
            .native_ims_register_profile_with_home_domain(
                &sim,
                None,
                Some("ims.custom.example"),
                true,
            )
            .unwrap();
        let identity = profile
            .resolve_subscriber_identity_from_snapshot(
                &sim.imsi,
                Some("impi@ims.custom.example"),
                Some("sip:impu@ims.custom.example"),
            )
            .unwrap();
        assert!(identity.uses_isim());
        assert_eq!(identity.private_identity(), "impi@ims.custom.example");
    }
}

pub fn resolve_access_identity(
    source: &str,
    derived: &[u8],
    private: &str,
    public: &str,
    domain: &str,
) -> Result<Vec<u8>, String> {
    if source != "isim" {
        return Ok(derived.to_vec());
    }
    let (private, public, domain) = (private.trim(), public.trim(), domain.trim());
    if private.is_empty() && public.is_empty() && domain.is_empty() {
        return Ok(derived.to_vec());
    }
    if private.is_empty() || public.is_empty() || domain.is_empty() {
        return Err("ISIM identity is incomplete".into());
    }
    if !private.contains('@')
        || private.bytes().any(|v| v.is_ascii_control())
        || public.bytes().any(|v| v.is_ascii_control())
        || domain.bytes().any(|v| v.is_ascii_control())
    {
        return Err("ISIM identity is invalid".into());
    }
    Ok(private.as_bytes().to_vec())
}

fn string(value: Option<&Value>, path: &str) -> Result<Option<String>, String> {
    match value.and_then(|v| v.pointer(path)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.clone())),
        _ => Err(format!("invalid template {path}")),
    }
}
fn number(value: Option<&Value>, path: &str) -> Result<Option<u64>, String> {
    value
        .and_then(|v| v.pointer(path))
        .map(|v| v.as_u64().ok_or_else(|| format!("invalid template {path}")))
        .transpose()
}
fn boolean(value: Option<&Value>, path: &str) -> Result<Option<bool>, String> {
    value
        .and_then(|v| v.pointer(path))
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| format!("invalid template {path}"))
        })
        .transpose()
}
fn strings(value: Option<&Value>, path: &str) -> Result<Option<Vec<String>>, String> {
    value
        .and_then(|v| v.pointer(path))
        .map(|v| {
            v.as_array()
                .ok_or_else(|| format!("invalid template {path}"))
                .and_then(|v| {
                    v.iter()
                        .map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| format!("invalid template {path}"))
                        })
                        .collect()
                })
        })
        .transpose()
}
fn algorithm_defaults(value: &CarrierOverride) -> (Vec<String>, Vec<String>) {
    let mut ike = vec![
        "aes256-sha512-prfsha512-modp2048",
        "aes256-sha256-prfsha256-modp2048",
        "aes128-sha256-prfsha256-modp2048",
        "aes256gcm16-prfsha384-modp2048",
        "aes128gcm16-prfsha256-modp2048",
    ];
    let mut esp = vec![
        "aes256-sha512",
        "aes256-sha256",
        "aes128-sha256",
        "aes256gcm16",
        "aes128gcm16",
    ];
    if value.enable_legacy_ciphers == Some(true) {
        if value.algorithm_policy.as_deref() == Some("legacy_prefer") {
            ike.insert(0, "aes128-sha1-prfsha1-modp2048");
            esp.insert(0, "aes128-sha1");
        } else {
            ike.push("aes128-sha1-prfsha1-modp2048");
            esp.push("aes128-sha1");
        }
    }
    (
        ike.into_iter().map(str::to_owned).collect(),
        esp.into_iter().map(str::to_owned).collect(),
    )
}
