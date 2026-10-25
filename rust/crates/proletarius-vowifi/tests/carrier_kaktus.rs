use proletarius_vowifi::{
    RequestedIpStack,
    carrier::{NativeAlgorithmProposals, NativeTunnelOverrides, Registry, SimIdentity},
    carrier_runtime_profile::RuntimeCarrierProfile,
};

// The published 23001 selector uses the generic TMobile_cz bundle for KAKTUS.
// Sources: https://github.com/iniwex5/ios-carrier-profiles/blob/main/selectors/23001.yaml
// and https://github.com/iniwex5/ios-carrier-profiles/blob/main/profiles/tmobile_cz.yaml.
// This fixture is synthetic and runs entirely offline; it proves configuration
// compatibility, not successful network authentication or subscriber entitlement.
#[test]
fn kaktus_resolves_tmobile_cz_native_tunnel_settings() {
    let registry = Registry::embedded().unwrap();
    let sim = SimIdentity {
        mcc: "230".into(),
        mnc: "01".into(),
        imsi: "230010000000000".into(),
        spn: "KAKTUS".into(),
        ..Default::default()
    };
    let carrier = registry.resolve(&sim).unwrap().unwrap();
    assert_eq!(carrier.id, "apple_tmobile_cz");
    assert_eq!(carrier.bundle, "TMobile_cz.bundle");

    let native = carrier
        .native_tunnel_config(&sim, &NativeTunnelOverrides::default())
        .unwrap();
    assert_eq!(native.apn, "ims");
    assert_eq!(native.ip_stack, RequestedIpStack::Ipv6);
    assert_eq!(native.epdg, "epdg.epc.mnc001.mcc230.pub.3gppnetwork.org");
    assert_eq!(native.epdg_port, 500);
    assert_eq!(
        native.identity(),
        b"0230010000000000@nai.epc.mnc001.mcc230.3gppnetwork.org"
    );
    assert_eq!(
        native.proposals,
        NativeAlgorithmProposals {
            ike: vec!["aes256-sha256-prfsha256-modp2048".into()],
            esp: vec!["aes256-sha256".into()],
        }
    );

    // Keep the compatibility runtime's interpretation aligned with the native
    // path; MNC remains two digits in selection and is padded only in the NAI.
    let runtime = RuntimeCarrierProfile::expand(&carrier, &sim, &registry.revision).unwrap();
    assert_eq!(runtime.mcc, "230");
    assert_eq!(runtime.mnc, "01");
    assert_eq!(runtime.apn.as_deref(), Some("ims"));
    assert_eq!(runtime.ip_stack.as_deref(), Some("ipv6"));
    assert_eq!(runtime.ike_encryption.as_deref(), Some("aes256"));
    assert_eq!(runtime.ike_integrity.as_deref(), Some("sha256"));
    assert_eq!(runtime.ike_prf.as_deref(), Some("sha256"));
    assert_eq!(runtime.ike_dh_group, 14);
    assert_eq!(runtime.child_encryption.as_deref(), Some("aes256"));
    assert_eq!(runtime.child_integrity.as_deref(), Some("sha256"));
    assert!(runtime.template_ike_proposal_exact);
    assert!(runtime.template_esp_proposal_exact);
}
