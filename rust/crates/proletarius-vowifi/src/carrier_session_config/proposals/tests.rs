use super::*;
use crate::carrier::{Registry, ResolvedCarrier, SimIdentity};
use crate::carrier_overrides::{CarrierOverride, OverrideSet};
fn selected() -> (SimIdentity, ResolvedCarrier) {
    let sim = SimIdentity {
        mcc: "302".into(),
        mnc: "720".into(),
        imsi: "302720123456789".into(),
        ..Default::default()
    };
    let carrier = Registry::embedded()
        .unwrap()
        .resolve(&sim)
        .unwrap()
        .unwrap();
    (sim, carrier)
}
#[test]
fn rogers_explicit_lists_replace_unsupported_template_before_parsing() {
    let (sim, carrier) = selected();
    assert!(carrier.native_algorithm_proposals().is_err());
    let overrides = NativeTunnelOverrides {
        ike_proposals: Some(vec!["aes256-sha256-prfsha256-modp2048".into()]),
        esp_proposals: Some(vec!["aes128-sha256".into()]),
        ..Default::default()
    };
    let config = carrier.native_tunnel_config(&sim, &overrides).unwrap();
    assert_eq!(config.proposals.ike, overrides.ike_proposals.unwrap());
    assert_eq!(config.proposals.esp, overrides.esp_proposals.unwrap());
}
#[test]
fn raw_template_compilation_is_strict_and_each_explicit_side_is_independent() {
    let (sim, mut c) = selected();
    let ike = vec!["aes256-sha256-prfsha256-modp2048".to_owned()];
    let esp = vec!["aes128-sha256".to_owned()];
    assert!(
        c.native_tunnel_config(
            &sim,
            &NativeTunnelOverrides {
                esp_proposals: Some(esp.clone()),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        c.native_tunnel_config(
            &sim,
            &NativeTunnelOverrides {
                ike_proposals: Some(ike.clone()),
                ..Default::default()
            }
        )
        .is_ok()
    );
    c.configuration["ims"]["ike"]["proposals"][0]["dh_groups"] = serde_json::json!([14]);
    c.configuration["ims"]["ike"]["child_sa"]["proposals"][0]["integrity"] =
        serde_json::json!(["sha384"]);
    assert!(
        c.native_tunnel_config(
            &sim,
            &NativeTunnelOverrides {
                ike_proposals: Some(ike.clone()),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        c.native_tunnel_config(
            &sim,
            &NativeTunnelOverrides {
                esp_proposals: Some(esp),
                ..Default::default()
            }
        )
        .is_ok()
    );
    assert!(c.native_algorithm_proposals().is_err());
}
#[test]
fn session_nonexact_profiles_use_executable_go_defaults_but_explicit_errors_remain_errors() {
    for (mcc, mnc) in [("302", "720"), ("228", "01"), ("231", "06")] {
        let sim = SimIdentity {
            mcc: mcc.into(),
            mnc: mnc.into(),
            imsi: format!("{mcc}{mnc}1234567890"),
            ..Default::default()
        };
        let c = Registry::embedded()
            .unwrap()
            .resolve(&sim)
            .unwrap()
            .unwrap();
        let (config, _) = c
            .native_session_tunnel_config(&sim, None, "", None)
            .unwrap();
        assert_eq!(config.proposals.ike, generic_ike(), "{mcc}/{mnc}");
        assert_eq!(config.proposals.esp, generic_esp());
    }
    let (sim, c) = selected();
    for invalid in [Vec::new(), vec!["aes256-sha256-modp3072".into()]] {
        let v = CarrierOverride {
            ike_proposals: Some(invalid),
            ..Default::default()
        };
        assert!(
            c.native_session_tunnel_config(&sim, Some(&v), "", None)
                .is_err()
        );
    }
}
#[test]
fn partial_explicit_and_algorithm_policies_keep_the_go_order() {
    let (sim, c) = selected();
    for side in [
        "ike_proposals: [aes128-sha256-modp2048]",
        "esp_proposals: [aes128-sha256]",
    ] {
        let yaml = format!("carrier_overrides:\n  '302/720': {{{side}}}\n");
        let set = OverrideSet::parse(yaml.as_bytes()).unwrap();
        let value = set.get("302", "720").unwrap();
        let (config, _) = c
            .native_session_tunnel_config(&sim, Some(value), "", None)
            .unwrap();
        assert_eq!(
            config.proposals.ike,
            value.ike_proposals.clone().unwrap_or_else(generic_ike)
        );
        assert_eq!(
            config.proposals.esp,
            value.esp_proposals.clone().unwrap_or_else(generic_esp)
        );
    }
    for (policy, enabled, first, last) in [
        (
            "strict",
            false,
            "aes256-sha512-prfsha512-modp2048",
            "aes128gcm16-prfsha256-modp2048",
        ),
        (
            "balanced",
            true,
            "aes256-sha512-prfsha512-modp2048",
            "aes128-sha1-prfsha1-modp2048",
        ),
        (
            "legacy_prefer",
            true,
            "aes128-sha1-prfsha1-modp2048",
            "aes128gcm16-prfsha256-modp2048",
        ),
    ] {
        let value = CarrierOverride {
            algorithm_policy: Some(policy.into()),
            enable_legacy_ciphers: Some(enabled),
            ..Default::default()
        };
        let (config, _) = c
            .native_session_tunnel_config(&sim, Some(&value), "", None)
            .unwrap();
        assert_eq!(config.proposals.ike.first().unwrap(), first);
        assert_eq!(config.proposals.ike.last().unwrap(), last);
    }
}
