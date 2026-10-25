//! Reviewed literals from upstream 09b8ae86; no regenerated Rust expectations.
use proletarius_vowifi::{
    carrier::{Registry, SimIdentity},
    carrier_runtime_profile::RuntimeCarrierProfile,
};
use serde_json::Value;

fn sim(mcc: &str, mnc: &str) -> SimIdentity {
    SimIdentity {
        mcc: mcc.into(),
        mnc: mnc.into(),
        ..Default::default()
    }
}

#[test]
fn every_current_selector_references_its_exact_profile_identity() {
    let document: Value =
        serde_json::from_str(include_str!("../../../carrier-registry.json")).unwrap();
    assert_eq!(
        document["revision"],
        "09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25"
    );
    for (plmn, manifest) in document["selectors"].as_object().unwrap() {
        assert_eq!(manifest["plmn"], *plmn);
        for candidate in manifest["profiles"].as_array().unwrap() {
            let path = candidate["path"].as_str().unwrap();
            let name = path
                .strip_prefix("profiles/")
                .unwrap()
                .strip_suffix(".yaml")
                .unwrap();
            assert!(!name.contains('/'));
            assert_eq!(
                document["profiles"][name]["id"], candidate["id"],
                "{plmn}: {path}"
            );
        }
    }
}

#[test]
fn new_plmns_and_retained_legacy_plmns_preserve_mnc_widths() {
    let registry = Registry::embedded().unwrap();
    for (mcc, mnc, id) in [
        ("280", "22", "apple_cablenet_cy"),
        ("740", "00", "apple_movistar_ec"),
        ("740", "005", "apple_movistar_ec"),
        ("234", "87", "lebara_uk"),
    ] {
        let carrier = registry.resolve(&sim(mcc, mnc)).unwrap().unwrap();
        assert_eq!(carrier.id, id);
        assert_eq!(carrier.selector, format!("{mcc}{mnc}"));
    }
    let historical: Vec<Value> = serde_json::from_str(include_str!(
        "../../../testdata/carrier-history/carrier-cases.json"
    ))
    .unwrap();
    for (mcc, mnc) in [
        ("302", "660"),
        ("310", "050"),
        ("310", "770"),
        ("311", "530"),
        ("466", "05"),
    ] {
        let case = historical
            .iter()
            .find(|case| {
                case["sim"]["mcc"] == mcc
                    && case["sim"]["mnc"] == mnc
                    && case["id"] != ""
                    && case["rejected"] == false
            })
            .unwrap();
        let identity: SimIdentity = serde_json::from_value(case["sim"].clone()).unwrap();
        assert_eq!(registry.resolve(&identity).unwrap().unwrap().id, case["id"]);
    }
}

#[test]
fn reviewed_updated_ims_values_reach_runtime_configuration() {
    let registry = Registry::embedded().unwrap();
    let identity = sim("234", "87");
    let carrier = registry.resolve(&identity).unwrap().unwrap();
    let profile = RuntimeCarrierProfile::expand(&carrier, &identity, &registry.revision).unwrap();
    assert_eq!(profile.id, "lebara_uk");
    // The new source advertises IPv6; the reviewed legacy selection keeps IPv4.
    let source: Value = serde_yaml::from_str(include_str!(
        "../../../../data/carriers/profiles/lebara_uk.yaml"
    ))
    .unwrap();
    assert_eq!(source["common"]["ims"]["ip_stack"], "ipv6");
    assert_eq!(profile.ip_stack.as_deref(), Some("ipv4"));
    assert!(profile.use_ipsec);
    assert_eq!(profile.ike_dh_group, 14);
    let carrier = registry.resolve(&sim("454", "00")).unwrap().unwrap();
    assert_eq!(carrier.id, "apple_csl_hk");
    assert_eq!(carrier.configuration["ims"]["register"]["use_ipsec"], true);
    assert_eq!(
        carrier.configuration["ims"]["signaling"]["max_udp_message_size"],
        3000
    );
}

#[test]
fn verizon_ting_retains_resolvable_historical_identity_until_impi_user_is_supported() {
    let registry = Registry::embedded().unwrap();
    let identity = SimIdentity {
        mcc: "311".into(),
        mnc: "480".into(),
        gid2: "A9".into(),
        imsi: "311480123456789".into(),
        ..Default::default()
    };
    let carrier = registry.resolve(&identity).unwrap().unwrap();
    assert_eq!(carrier.id, "apple_verizon_ting_lte_us");
    let profile = RuntimeCarrierProfile::expand(&carrier, &identity, &registry.revision).unwrap();
    assert_eq!(
        profile.local_identifier.as_deref(),
        Some("0$imsi@nai.epc.mnc$mnc.mcc$mcc.3gppnetwork.org")
    );
    let (config, _) = carrier
        .native_session_tunnel_config(&identity, None, "", None)
        .unwrap();
    assert_eq!(
        config.identity(),
        b"0311480123456789@nai.epc.mnc480.mcc311.3gppnetwork.org"
    );
    assert!(!config.identity().contains(&b'$'));
    assert!(
        !config
            .ike_local_identity()
            .unwrap_or(config.identity())
            .contains(&b'$')
    );
}

#[test]
fn all_historical_selections_and_protected_effective_profiles_are_retained() {
    let historical: Registry = serde_json::from_str(include_str!(
        "../../../testdata/carrier-history/carrier-registry.json"
    ))
    .unwrap();
    let current = Registry::embedded().unwrap();
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../testdata/carrier-history/carrier-cases.json"
    ))
    .unwrap();
    let overlay: Value = serde_json::from_str(include_str!(
        "../../../../data/carriers/compatibility/registry-overlay.json"
    ))
    .unwrap();
    let retained = overlay["historical_case_indices"].as_array().unwrap();
    assert_eq!(retained.len(), 43);
    let protected = [
        "apple_orange_uk",
        "apple_vodafone_uk",
        "apple_o2_giffgaff_uk",
        "apple_hutchison_uk",
        "apple_hutchison_hk",
        "apple_tmobile_uk",
        "apple_o2_uk",
    ];
    let mut exact_profiles = 0;
    for (index, case) in cases.iter().enumerate() {
        let identity: SimIdentity = serde_json::from_value(case["sim"].clone()).unwrap();
        let selected = current.resolve(&identity);
        if case["rejected"] == true {
            assert!(selected.is_err(), "historical rejection {index}");
            continue;
        }
        let selected = selected.unwrap();
        assert_eq!(
            selected.as_ref().map(|c| c.id.as_str()).unwrap_or(""),
            case["id"].as_str().unwrap(),
            "historical selection {index}"
        );
        let Some(carrier) = selected else { continue };
        assert_eq!(
            carrier.selector, case["selector"],
            "historical raw selector {index}"
        );
        // Request-level ePDG and port still take precedence for every legacy SIM.
        let mut tunnel_identity: SimIdentity = serde_json::from_value(case["sim"].clone()).unwrap();
        tunnel_identity.imsi = format!("{}{}1234567890", identity.mcc, identity.mnc);
        let (config, _) = carrier
            .native_session_tunnel_config(
                &tunnel_identity,
                None,
                "custom.example.invalid",
                Some(4444),
            )
            .unwrap();
        assert_eq!(config.epdg, "custom.example.invalid");
        assert_eq!(config.epdg_port, 4444);
        if retained.contains(&serde_json::json!(index)) || protected.contains(&carrier.id.as_str())
        {
            let original = historical.resolve(&identity).unwrap().unwrap();
            assert_eq!(
                carrier.selector, original.selector,
                "historical selector {index}"
            );
            let mut expected = serde_json::to_value(
                RuntimeCarrierProfile::expand(&original, &identity, &historical.revision).unwrap(),
            )
            .unwrap();
            let mut actual = serde_json::to_value(
                RuntimeCarrierProfile::expand(&carrier, &identity, &current.revision).unwrap(),
            )
            .unwrap();
            expected
                .as_object_mut()
                .unwrap()
                .remove("registry_revision");
            actual.as_object_mut().unwrap().remove("registry_revision");
            assert_eq!(
                actual, expected,
                "retained complete effective profile {index}"
            );
            exact_profiles += 1;
        }
    }
    assert!(
        exact_profiles > 43,
        "protected carriers must be covered in addition to legacy rules"
    );
}
