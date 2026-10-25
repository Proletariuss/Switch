//! Rust-owned expansion of the pinned Apple carrier bundle into the compact
//! profile consumed by the Go IMS compatibility runtime. This keeps template
//! selection and field interpretation on one side of the process boundary;
//! Go-only recovery mode retains its embedded resolver.

use serde::Serialize;
use serde_json::Value;

use crate::carrier::{ResolvedCarrier, SimIdentity};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuntimeCarrierProfile {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub mcc: String,
    pub mnc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_mcc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_mnc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ims_domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ims_transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_phone_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epdg_candidates: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_stack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ike_encryption: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ike_integrity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ike_prf: Option<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub ike_dh_group: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub ike_lifetime: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_encryption: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_integrity: Option<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub child_dh_group: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub child_lifetime: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub natt_keepalive: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub dpd_interval: i32,
    #[serde(skip_serializing_if = "is_false")]
    pub disable_dpd: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eap_identity_template: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub use_ipsec: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ims_auth_algorithm: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub validate_remote_certificate: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub disable_switch_to_natt_port: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector_raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub register_contact_params: Option<Vec<String>>,
    #[serde(skip_serializing_if = "is_false")]
    pub support_p_early_media: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub early_media_needs_header: bool,
    /// Parser capability metadata used by Go proposal selection. It is not a
    /// negotiated-algorithm claim and is intentionally separate from the
    /// public carrier template itself.
    #[serde(skip_serializing_if = "is_false")]
    pub template_ike_proposal_exact: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub template_esp_proposal_exact: bool,
    /// Complete user-supplied compatibility template. This is intentionally
    /// separate from the pinned registry and from native IKE capabilities.
    /// Keeping the JSON intact preserves ordered proposals and explicit false
    /// values across the Rust-to-Go runtime bridge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vowifi_template: Option<Value>,
}

impl RuntimeCarrierProfile {
    pub fn standard(sim: &SimIdentity, _revision: &str) -> Self {
        let mut profile = Self {
            id: format!("auto-{}-{}", sim.mcc, sim.mnc),
            name: format!("3GPP Auto ({}/{})", sim.mcc, sim.mnc),
            source: Some("3gpp-default".into()),
            mcc: sim.mcc.clone(),
            mnc: sim.mnc.clone(),
            route_mcc: Some(sim.mcc.clone()),
            route_mnc: Some(sim.mnc.clone()),
            ims_domain: None,
            ims_transport: None,
            apn: Some("ims".into()),
            call_phone_context: None,
            epdg_candidates: None,
            ip_stack: Some("ipv4v6".into()),
            ike_encryption: Some("aes128".into()),
            ike_integrity: Some("sha256".into()),
            ike_prf: Some("sha256".into()),
            ike_dh_group: 14,
            ike_lifetime: 80_000,
            child_encryption: Some("aes128".into()),
            child_integrity: Some("sha256".into()),
            child_dh_group: 14,
            child_lifetime: 86_400,
            natt_keepalive: 20,
            dpd_interval: 120,
            disable_dpd: false,
            local_identifier: Some("0$imsi@nai.epc.mnc$mnc.mcc$mcc.3gppnetwork.org".into()),
            eap_identity_template: None,
            use_ipsec: true,
            ims_auth_algorithm: Some("akav1-md5".into()),
            validate_remote_certificate: false,
            disable_switch_to_natt_port: false,
            bundle: None,
            selector_raw: None,
            registry_revision: None,
            register_contact_params: None,
            support_p_early_media: false,
            early_media_needs_header: false,
            template_ike_proposal_exact: false,
            template_esp_proposal_exact: false,
            vowifi_template: None,
        };
        // These values are intentionally the same default profile used by the
        // compatibility resolver when no reviewed bundle matches.
        profile.child_lifetime = 86_400;
        profile
    }

    pub fn expand(
        carrier: &ResolvedCarrier,
        sim: &SimIdentity,
        revision: &str,
    ) -> Result<Self, String> {
        let mut profile = Self::standard(sim, revision);
        profile.id.clone_from(&carrier.id);
        profile.name = carrier
            .carrier_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| display_name(&carrier.bundle));
        profile.source = Some("builtin".into());
        profile.registry_revision = Some(revision.to_owned());
        profile.bundle = nonempty(&carrier.bundle).map(str::to_owned);
        profile.selector_raw = nonempty(&carrier.selector).map(str::to_owned);

        set_string(&carrier.configuration, "/ims/apn", &mut profile.apn)?;
        set_string(
            &carrier.configuration,
            "/ims/ip_stack",
            &mut profile.ip_stack,
        )?;
        set_string(
            &carrier.configuration,
            "/ims/transport",
            &mut profile.ims_transport,
        )?;

        if let Some(address) = epdg_address(&carrier.configuration)? {
            profile.epdg_candidates = Some(vec![address.to_owned()]);
        }

        let ike_proposals = objects(&carrier.configuration, "/ims/ike/proposals")?;
        if let Some(proposals) = ike_proposals {
            if let Some(first) = proposals.first() {
                let encryption = strings(first, "encryption")?;
                let integrity = strings(first, "integrity")?;
                let prf = strings(first, "prf")?;
                let groups = integers(first, "dh_groups")?;
                profile.template_ike_proposal_exact = proposals.len() == 1
                    && encryption.len() == 1
                    && integrity.len() == 1
                    && prf.len() == 1
                    && groups.len() == 1;
                set_first(&mut profile.ike_encryption, &encryption);
                set_first(&mut profile.ike_integrity, &integrity);
                set_first(&mut profile.ike_prf, &prf);
                if let Some(group) = groups.first() {
                    profile.ike_dh_group = *group;
                }
                set_integer(first, "lifetime_seconds", &mut profile.ike_lifetime)?;
            }
        }

        set_integer(
            &carrier.configuration,
            "/ims/ike/nat_keepalive_seconds",
            &mut profile.natt_keepalive,
        )?;
        set_integer(
            &carrier.configuration,
            "/ims/ike/dpd/interval_seconds",
            &mut profile.dpd_interval,
        )?;
        if let Some(enabled) = optional_bool(&carrier.configuration, "/ims/ike/dpd/enabled")? {
            profile.disable_dpd = !enabled;
        }
        set_string(
            &carrier.configuration,
            "/ims/ike/local_identifier",
            &mut profile.local_identifier,
        )?;
        set_string(
            &carrier.configuration,
            "/ims/ike/username",
            &mut profile.eap_identity_template,
        )?;
        if let Some(value) = optional_bool(
            &carrier.configuration,
            "/ims/ike/validate_remote_certificate",
        )? {
            profile.validate_remote_certificate = value;
        }
        if let Some(value) = optional_bool(
            &carrier.configuration,
            "/ims/ike/disable_switch_to_natt_port",
        )? {
            profile.disable_switch_to_natt_port = value;
        }

        let child_path = "/ims/ike/child_sa/proposals";
        let child_proposals = objects(&carrier.configuration, child_path)?;
        if let Some(proposals) = child_proposals {
            if let Some(first) = proposals.first() {
                let encryption = strings(first, "encryption")?;
                let integrity = strings(first, "integrity")?;
                let esn = optional_string(first, "esn")?;
                profile.template_esp_proposal_exact = proposals.len() == 1
                    && encryption.len() == 1
                    && integrity.len() == 1
                    && esp_esn_compatible(esn);
                set_first(&mut profile.child_encryption, &encryption);
                set_first(&mut profile.child_integrity, &integrity);
                if let Some(group) = optional_integer(first, "rekey_pfs_group")? {
                    if group > 0 {
                        profile.child_dh_group = group;
                    }
                }
                set_integer(first, "lifetime_seconds", &mut profile.child_lifetime)?;
            }
        }
        set_integer(
            &carrier.configuration,
            "/ims/ike/child_sa/lifetime_seconds",
            &mut profile.child_lifetime,
        )?;
        if let Some(value) = optional_bool(&carrier.configuration, "/ims/register/use_ipsec")? {
            profile.use_ipsec = value;
        }
        let contact = objects(
            &carrier.configuration,
            "/ims/signaling/contact/additional_contact_params",
        )?;
        if let Some(entries) = contact {
            let mut params = Vec::new();
            for entry in entries {
                if optional_string(entry, "selector")?
                    .is_some_and(|selector| selector.to_ascii_uppercase().contains("REGISTER"))
                {
                    for value in strings(entry, "params")? {
                        if !params.contains(&value) {
                            params.push(value);
                        }
                    }
                }
            }
            if !params.is_empty() {
                profile.register_contact_params = Some(params);
            }
        }
        if let Some(value) = optional_bool(
            &carrier.configuration,
            "/ims/signaling/support_p_early_media",
        )? {
            profile.support_p_early_media = value;
        }
        if let Some(value) = optional_bool(
            &carrier.configuration,
            "/ims/signaling/early_media_needs_header",
        )? {
            profile.early_media_needs_header = value;
        }
        profile.apply_three_hk_user_template(sim)?;
        Ok(profile)
    }

    fn apply_three_hk_user_template(&mut self, sim: &SimIdentity) -> Result<(), String> {
        // A shared home PLMN must not override a more specific MVNO match.
        // 45404 also resolves to Hutchison, but is outside the supplied file.
        if self.id != "apple_hutchison_hk"
            || sim.mcc != "454"
            || !matches!(sim.mnc.as_str(), "3" | "03" | "003")
        {
            return Ok(());
        }
        let template: Value = serde_json::from_str(include_str!(
            "../../../../data/carriers/user_templates/3_hk.json"
        ))
        .map_err(|_| "invalid embedded 3_hk user template".to_owned())?;
        self.name = "3 HK / SIMworld".into();
        self.source = Some("user-template".into());
        set_string(&template, "/ike/apn", &mut self.apn)?;
        set_string(&template, "/ike/ip_stack", &mut self.ip_stack)?;
        if let Some(address) = optional_string(&template, "/ike/addr")? {
            self.epdg_candidates = Some(vec![address.to_owned()]);
        }
        // These compact fields describe the preferred candidate only. The
        // complete ordered offer is carried in vowifi_template below, so the
        // old single-proposal exactness hints must not survive this overlay.
        self.ike_encryption = Some("aes128".into());
        self.ike_integrity = Some("sha256".into());
        self.ike_prf = Some("sha256".into());
        self.ike_dh_group = 14;
        self.child_encryption = Some("aes256".into());
        self.child_integrity = Some("sha256".into());
        self.child_dh_group = 14;
        self.template_ike_proposal_exact = false;
        self.template_esp_proposal_exact = false;
        self.vowifi_template = Some(template);
        Ok(())
    }
}

fn display_name(bundle: &str) -> String {
    bundle
        .strip_suffix(".bundle")
        .unwrap_or(bundle)
        .replace('_', " ")
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn epdg_address(configuration: &Value) -> Result<Option<&str>, String> {
    // yaml.v3 decodes every configured candidate into a typed struct before
    // expansion. Validate the complete shape first so an invalid later item
    // cannot be hidden by a valid legacy address with higher precedence.
    if let Some(candidates) = configuration.pointer("/ims/epdg/candidates") {
        match candidates {
            Value::Null => {}
            Value::Array(entries) => {
                for entry in entries {
                    if !entry.is_object() {
                        return Err("carrier template has invalid /ims/epdg/candidates".into());
                    }
                    optional_string(entry, "address")?;
                }
            }
            _ => return Err("carrier template has invalid /ims/epdg/candidates".into()),
        }
    }
    for pointer in [
        "/ims/epdg/address",
        "/ims/epdg/candidates/0/address",
        "/ims/epdg/remote_certificate_hostname",
    ] {
        if let Some(value) = optional_string(configuration, pointer)? {
            let value = value.trim();
            if !value.is_empty() {
                return Ok(Some(value));
            }
        }
    }
    // Match the Go bundle decoder: a non-array candidates field or non-object
    // first candidate is malformed even when the legacy hostname is present.
    Ok(None)
}

fn set_string(
    configuration: &Value,
    pointer: &str,
    destination: &mut Option<String>,
) -> Result<(), String> {
    if let Some(value) = optional_string(configuration, pointer)? {
        if !value.is_empty() {
            *destination = Some(value.to_owned());
        }
    }
    Ok(())
}

fn optional_string<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match value.pointer(key).or_else(|| value.get(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!("carrier template has an invalid {key}")),
    }
}

fn optional_bool(value: &Value, key: &str) -> Result<Option<bool>, String> {
    match value.pointer(key).or_else(|| value.get(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("carrier template has an invalid {key}")),
    }
}

fn optional_integer(value: &Value, key: &str) -> Result<Option<i32>, String> {
    match value.pointer(key).or_else(|| value.get(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| format!("carrier template has an invalid {key}")),
        Some(_) => Err(format!("carrier template has an invalid {key}")),
    }
}

fn set_integer(configuration: &Value, pointer: &str, destination: &mut i32) -> Result<(), String> {
    if let Some(value) = optional_integer(configuration, pointer)? {
        if value > 0 {
            *destination = value;
        }
    }
    Ok(())
}

fn strings(value: &Value, key: &str) -> Result<Vec<String>, String> {
    match value.pointer(key).or_else(|| value.get(key)) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("carrier template has an invalid {key}"))
            })
            .collect(),
        Some(_) => Err(format!("carrier template has an invalid {key}")),
    }
}

fn integers(value: &Value, key: &str) -> Result<Vec<i32>, String> {
    match value.pointer(key).or_else(|| value.get(key)) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_i64()
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or_else(|| format!("carrier template has an invalid {key}"))
            })
            .collect(),
        Some(_) => Err(format!("carrier template has an invalid {key}")),
    }
}

fn objects<'a>(value: &'a Value, key: &str) -> Result<Option<Vec<&'a Value>>, String> {
    match value.pointer(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .is_object()
                    .then_some(value)
                    .ok_or_else(|| format!("carrier template has an invalid {key}"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(format!("carrier template has an invalid {key}")),
    }
}

fn set_first(destination: &mut Option<String>, values: &[String]) {
    if let Some(value) = values.first().filter(|value| !value.is_empty()) {
        *destination = Some(value.clone());
    }
}

fn esp_esn_compatible(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        None | Some("" | "auto" | "noesn")
    )
}

fn is_zero(value: &i32) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::carrier::Registry;

    #[test]
    fn latest_selector_carrier_name_reaches_runtime_profile_display() {
        let registry = Registry::embedded().unwrap();
        let sim = SimIdentity {
            mcc: "310".into(),
            mnc: "260".into(),
            gid1: "1A5300".into(),
            ..Default::default()
        };
        let carrier = registry.resolve(&sim).unwrap().unwrap();
        let profile = RuntimeCarrierProfile::expand(&carrier, &sim, &registry.revision).unwrap();
        assert_eq!(profile.id, "apple_tmobile_mvno_us");
        assert_eq!(profile.name, "TMobile MVNO US");
        assert_eq!(profile.selector_raw.as_deref(), Some("310260_GID1-1A53"));
    }

    #[test]
    fn three_hk_user_template_preserves_complete_ordered_runtime_configuration() {
        let registry = Registry::embedded().unwrap();
        let sim = SimIdentity {
            mcc: "454".into(),
            mnc: "03".into(),
            ..Default::default()
        };
        let carrier = registry.resolve(&sim).unwrap().unwrap();
        let profile = RuntimeCarrierProfile::expand(&carrier, &sim, &registry.revision).unwrap();
        assert_eq!(profile.id, "apple_hutchison_hk");
        assert_eq!(profile.name, "3 HK / SIMworld");
        assert_eq!(profile.source.as_deref(), Some("user-template"));
        assert_eq!(profile.bundle.as_deref(), Some("Hutchison_hk.bundle"));
        assert_eq!(
            profile.epdg_candidates,
            Some(vec!["wlan.three.com.hk".into()])
        );
        assert_eq!(profile.ip_stack.as_deref(), Some("ipv4"));
        assert_eq!(profile.apn.as_deref(), Some("ims"));
        assert_eq!(profile.ike_encryption.as_deref(), Some("aes128"));
        assert_eq!(profile.ike_integrity.as_deref(), Some("sha256"));
        assert_eq!(profile.ike_prf.as_deref(), Some("sha256"));
        assert_eq!(profile.ike_dh_group, 14);
        assert_eq!(profile.child_encryption.as_deref(), Some("aes256"));
        assert_eq!(profile.child_integrity.as_deref(), Some("sha256"));
        assert_eq!(profile.child_dh_group, 14);
        assert!(!profile.template_ike_proposal_exact);
        assert!(!profile.template_esp_proposal_exact);
        let serialized = serde_json::to_value(&profile).unwrap();
        let supplied: Value = serde_json::from_str(include_str!(
            "../../../../data/carriers/user_templates/3_hk.json"
        ))
        .unwrap();
        assert_eq!(serialized["vowifi_template"], supplied);
        assert_eq!(
            serialized["vowifi_template"]["ike"]["proposals"],
            serde_json::json!([
                "aes128-sha256-modp2048",
                "aes256-sha256-modp2048",
                "aes256-sha512-modp2048"
            ])
        );
        assert_eq!(
            serialized["vowifi_template"]["eap"]["identity_source"],
            "isim"
        );
        assert_eq!(
            serialized["vowifi_template"]["ike"]["cp_in_first_auth"],
            true
        );
        assert_eq!(
            serialized["vowifi_template"]["ike"]["cp_in_final_auth"],
            true
        );
        assert_eq!(
            serialized["vowifi_template"]["ims"]["sec_agree_mode"],
            "off"
        );
        assert_eq!(serialized["vowifi_template"]["blocked"], false);
        // The MIT registry remains untouched; the full user JSON belongs only
        // to the compatibility runtime profile, not native tunnel selection.
        assert_eq!(carrier.configuration["ims"]["ip_stack"], "ipv4v6");
    }

    #[test]
    fn three_hk_template_scope_accepts_mnc_spelling_only_after_carrier_selection() {
        let registry = Registry::embedded().unwrap();
        let mut sim = SimIdentity {
            mcc: "454".into(),
            mnc: "03".into(),
            ..Default::default()
        };
        let mut carrier = registry.resolve(&sim).unwrap().unwrap();
        for mnc in ["3", "03", "003"] {
            sim.mnc = mnc.into();
            let profile =
                RuntimeCarrierProfile::expand(&carrier, &sim, &registry.revision).unwrap();
            assert!(profile.vowifi_template.is_some(), "mnc={mnc}");
        }
        for mnc in ["04", "030", "0003", "3 ", "+3"] {
            sim.mnc = mnc.into();
            let profile =
                RuntimeCarrierProfile::expand(&carrier, &sim, &registry.revision).unwrap();
            assert!(profile.vowifi_template.is_none(), "mnc={mnc}");
        }
        sim.mnc = "03".into();
        sim.mcc = "453".into();
        assert!(
            RuntimeCarrierProfile::expand(&carrier, &sim, &registry.revision)
                .unwrap()
                .vowifi_template
                .is_none()
        );
        sim.mcc = "454".into();
        carrier.id = "apple_dentwireless".into();
        assert!(
            RuntimeCarrierProfile::expand(&carrier, &sim, &registry.revision)
                .unwrap()
                .vowifi_template
                .is_none()
        );
    }

    #[test]
    fn three_hk_template_does_not_override_specific_mvnos_or_other_home_plmns() {
        let registry = Registry::embedded().unwrap();
        for (mcc, mnc, gid1, gid2, iccid, id) in [
            ("454", "03", "3597BFD533", "", "", "apple_dentwireless"),
            ("454", "03", "0009", "", "", "apple_hutchison_hkbn_hk"),
            ("454", "03", "", "", "8985203026", "apple_hutchison_hkbn_hk"),
            ("454", "03", "0227", "02280", "", "apple_multibyte"),
            ("454", "03", "", "", "89852245280", "apple_redteamobile"),
            ("454", "03", "AAAA", "", "", "apple_shinetown"),
            ("454", "03", "", "", "8985235", "apple_webbing"),
            ("454", "04", "", "", "", "apple_hutchison_hk"),
            ("234", "15", "", "", "", "apple_vodafone_uk"),
        ] {
            let sim = SimIdentity {
                mcc: mcc.into(),
                mnc: mnc.into(),
                gid1: gid1.into(),
                gid2: gid2.into(),
                iccid: iccid.into(),
                ..Default::default()
            };
            let carrier = registry.resolve(&sim).unwrap().unwrap();
            assert_eq!(carrier.id, id);
            let profile =
                RuntimeCarrierProfile::expand(&carrier, &sim, &registry.revision).unwrap();
            assert!(profile.vowifi_template.is_none(), "{id}/{mnc}");
            assert_eq!(profile.source.as_deref(), Some("builtin"));
            assert!(
                serde_json::to_value(profile)
                    .unwrap()
                    .get("vowifi_template")
                    .is_none()
            );
        }
    }

    #[test]
    fn expands_clexcel_and_giffgaff_runtime_fields_from_pinned_templates() {
        let registry = Registry::embedded().unwrap();
        let ctexcel = SimIdentity {
            mcc: "234".into(),
            mnc: "33".into(),
            ..Default::default()
        };
        let carrier = registry.resolve(&ctexcel).unwrap().unwrap();
        let profile =
            RuntimeCarrierProfile::expand(&carrier, &ctexcel, &registry.revision).unwrap();
        assert_eq!(profile.id, "apple_orange_uk");
        assert_eq!(profile.ip_stack.as_deref(), Some("ipv6"));
        assert_eq!(profile.dpd_interval, 600);
        assert!(profile.disable_dpd);
        assert_eq!(profile.child_lifetime, 86_400);
        assert!(profile.support_p_early_media);
        assert!(profile.early_media_needs_header);

        let giffgaff = SimIdentity {
            mcc: "234".into(),
            mnc: "10".into(),
            gid1: "508FFFFF00".into(),
            ..Default::default()
        };
        let carrier = registry.resolve(&giffgaff).unwrap().unwrap();
        let profile =
            RuntimeCarrierProfile::expand(&carrier, &giffgaff, &registry.revision).unwrap();
        assert_eq!(profile.id, "apple_o2_giffgaff_uk");
        assert_eq!(profile.ike_encryption.as_deref(), Some("aes256"));
        assert_eq!(profile.ike_integrity.as_deref(), Some("sha512"));
        assert!(profile.template_ike_proposal_exact);
        assert!(profile.template_esp_proposal_exact);
    }

    #[test]
    fn expands_vodafone_uk_and_preserves_talkmobile_selector_precedence() {
        let registry = Registry::embedded().unwrap();
        let vodafone = SimIdentity {
            mcc: "234".into(),
            mnc: "15".into(),
            ..Default::default()
        };
        let carrier = registry.resolve(&vodafone).unwrap().unwrap();
        let profile =
            RuntimeCarrierProfile::expand(&carrier, &vodafone, &registry.revision).unwrap();
        assert_eq!(profile.id, "apple_vodafone_uk");
        assert_eq!(
            profile.epdg_candidates.as_ref().unwrap(),
            &["epdg.epc.mnc015.mcc234.pub.3gppnetwork.org".to_owned()]
        );
        assert_eq!(profile.ip_stack.as_deref(), Some("ipv4"));
        assert_eq!(profile.ike_encryption.as_deref(), Some("aes256"));
        assert_eq!(profile.ike_integrity.as_deref(), Some("sha256"));
        assert_eq!(profile.eap_identity_template.as_deref(), None);
        assert!(!profile.validate_remote_certificate);

        let talkmobile = SimIdentity {
            mcc: "234".into(),
            mnc: "15".into(),
            gid1: "C1AB".into(),
            ..Default::default()
        };
        assert_eq!(
            registry.resolve(&talkmobile).unwrap().unwrap().id,
            "apple_talkmobile_uk"
        );
    }

    #[test]
    fn expands_three_uk_identity_and_ims_transport_separately() {
        let registry = Registry::embedded().unwrap();
        let sim = SimIdentity {
            mcc: "234".into(),
            mnc: "20".into(),
            imsi: "234201234567890".into(),
            ..Default::default()
        };
        let carrier = registry.resolve(&sim).unwrap().unwrap();
        let profile = RuntimeCarrierProfile::expand(&carrier, &sim, &registry.revision).unwrap();
        assert_eq!(profile.id, "apple_hutchison_uk");
        assert_eq!(profile.ims_transport.as_deref(), Some("tcp"));
        assert_eq!(
            profile.eap_identity_template.as_deref(),
            Some("0$imsi@nai.epc.mnc020.mcc234.3gppnetwork.org")
        );
        assert_eq!(profile.ike_encryption.as_deref(), Some("aes256"));
        assert_eq!(profile.ike_integrity.as_deref(), Some("sha256"));
        assert_eq!(profile.ike_dh_group, 14);
        assert_eq!(profile.child_encryption.as_deref(), Some("aes128"));
        assert_eq!(profile.child_integrity.as_deref(), Some("sha256"));
        assert!(profile.validate_remote_certificate);
    }

    #[test]
    fn malformed_runtime_fields_fail_closed() {
        let carrier = ResolvedCarrier {
            id: "fixture".into(),
            bundle: "Fixture.bundle".into(),
            selector: String::new(),
            carrier_name: None,
            configuration: serde_json::json!({"ims":{"ip_stack":false}}),
        };
        let sim = SimIdentity {
            mcc: "234".into(),
            mnc: "33".into(),
            ..Default::default()
        };
        assert!(RuntimeCarrierProfile::expand(&carrier, &sim, "test").is_err());
    }

    #[test]
    fn malformed_later_epdg_candidate_fails_even_when_address_is_present() {
        let carrier = ResolvedCarrier {
            id: "fixture".into(),
            bundle: "Fixture.bundle".into(),
            selector: String::new(),
            carrier_name: None,
            configuration: serde_json::json!({
                "ims": {
                    "epdg": {
                        "address": "epdg.example.invalid",
                        "candidates": [{"address": "fallback.example.invalid"}, 42]
                    }
                }
            }),
        };
        let sim = SimIdentity {
            mcc: "234".into(),
            mnc: "33".into(),
            ..Default::default()
        };
        assert!(RuntimeCarrierProfile::expand(&carrier, &sim, "test").is_err());
    }
}
