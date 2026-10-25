use proletarius_vowifi::{
    carrier::{Registry, SimIdentity},
    carrier_runtime_profile::RuntimeCarrierProfile,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    registry_revision: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    sim: SimIdentity,
    matched: bool,
    id: String,
    epdg: String,
}

// Shared with Go: aliases already covered, exact MVNO identity, protected
// profiles, and network PLMNs that must not become inferred SIM selectors.
// No sockets, devices, credentials, or entitlement claims are involved.
#[test]
fn fixed_reference_preserves_effective_carrier_boundaries() {
    let fixture: Fixture = serde_json::from_str(include_str!(
        "../../../carrier-reference-boundary-cases.json"
    ))
    .unwrap();
    let registry = Registry::embedded().unwrap();
    // The independently reviewed boundary fixture retains its capture revision.
    assert_eq!(
        fixture.registry_revision,
        "cdba73e551810681f16b4cce0693c70e9e216c37"
    );
    assert_eq!(
        registry.revision,
        "09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25"
    );
    assert_eq!(fixture.cases.len(), 25);
    for case in fixture.cases {
        let selected = registry.resolve(&case.sim).unwrap();
        assert_eq!(selected.is_some(), case.matched, "{}", case.name);
        let profile = match selected {
            Some(selected) => {
                RuntimeCarrierProfile::expand(&selected, &case.sim, &registry.revision).unwrap()
            }
            None => RuntimeCarrierProfile::standard(&case.sim, &registry.revision),
        };
        let value = serde_json::to_value(profile).unwrap();
        let standard = format!(
            "epdg.epc.mnc{:0>3}.mcc{}.pub.3gppnetwork.org",
            case.sim.mnc, case.sim.mcc
        );
        let endpoint = value["epdg_candidates"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.as_str())
            .unwrap_or(&standard);
        assert_eq!(value["id"], case.id, "{}", case.name);
        assert_eq!(endpoint, case.epdg, "{}", case.name);
    }
}
