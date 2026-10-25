//! Expected profiles are computed once by the Go resolver, never by Rust replay.
use proletarius_vowifi::carrier::{Registry, ResolvedCarrier, SimIdentity};
use proletarius_vowifi::carrier_runtime_profile::RuntimeCarrierProfile;
use serde_json::{Value, json};
use std::collections::BTreeSet;

// The former Go decoder assigned scalar zero values to omitted JSON fields.
// Compare those same values semantically, preserving array contents/order and
// empty arrays. Top-level nil/empty slices have a separate exact assertion.
fn scalar_defaults(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields.iter_mut() {
                // Go stores device values as raw JSON, so explicit false/zero
                // entries there are meaningful and must remain exact.
                if key != "device" {
                    scalar_defaults(child);
                }
            }
            fields.retain(|_, child| {
                !(child.is_null()
                    || child.as_bool() == Some(false)
                    || child.as_str() == Some("")
                    || child.as_i64() == Some(0))
            });
        }
        Value::Array(items) => items.iter_mut().for_each(scalar_defaults),
        _ => {}
    }
}

#[test]
fn frozen_go_carrier_parameters_match_native_expansion() {
    let bytes = std::env::var("SWITCH_CARRIER_PARAMETERS_ORACLE")
        .map(|path| std::fs::read(path).expect("read explicit carrier parameter oracle"))
        .unwrap_or_else(|_| {
            include_bytes!("../../../testdata/retired-go-oracles/carrier-parameters.json").to_vec()
        });
    let fixture: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(fixture["schema_version"], 1);
    assert_eq!(fixture["case_count"], 2726);
    assert_eq!(fixture["rejected_count"], 3);
    let rows = fixture["cases"].as_array().unwrap();
    assert_eq!(rows.len(), 2726);
    let input: Vec<Value> = serde_json::from_str(include_str!(
        "../../../testdata/carrier-history/carrier-cases.json"
    ))
    .unwrap();
    assert_eq!(rows.len(), input.len());
    let registry: Registry = serde_json::from_str(include_str!(
        "../../../testdata/carrier-history/carrier-registry.json"
    ))
    .unwrap();
    assert_eq!(
        registry.revision,
        "cdba73e551810681f16b4cce0693c70e9e216c37"
    );
    let mut names = BTreeSet::new();
    let mut rejected = 0;
    for (index, row) in rows.iter().enumerate() {
        let name = row["name"].as_str().unwrap();
        assert!(names.insert(name), "duplicate carrier fixture name");
        assert_eq!(
            row["sim"], input[index]["sim"],
            "{name}: complete input corpus"
        );
        let sim: SimIdentity = serde_json::from_value(row["sim"].clone()).unwrap();
        let selected = registry.resolve(&sim);
        if row["rejected"] == true {
            assert_eq!(row["error_class"], "carrier_resolution_rejected");
            assert!(!row["error_text"].as_str().unwrap().is_empty());
            assert!(
                selected.is_err(),
                "{name}: rejected selector must not fall back"
            );
            assert!(row.get("runtime_profile").is_none());
            rejected += 1;
            continue;
        }
        assert_eq!(row["rejected"], false);
        assert!(row.get("error_class").is_none());
        let selected = selected.unwrap();
        assert_eq!(row["matched"], selected.is_some(), "{name}");
        let profile = match selected.as_ref() {
            Some(selected) => RuntimeCarrierProfile::expand(selected, &sim, &registry.revision)
                .unwrap_or_else(|error| panic!("{name}: {error}")),
            None => RuntimeCarrierProfile::standard(&sim, &registry.revision),
        };
        let mut actual = serde_json::to_value(&profile).unwrap();
        for key in ["epdg_candidates", "pcscf", "register_contact_params"] {
            let presence = match actual.get(key) {
                None | Some(Value::Null) => "nil",
                Some(Value::Array(values)) if values.is_empty() => "empty",
                Some(Value::Array(_)) => "nonempty",
                _ => panic!("{name}: invalid slice {key}"),
            };
            assert_eq!(row["slice_presence"][key], presence, "{name}: {key}");
        }
        let mut expected = row["runtime_profile"].clone();
        assert!(expected.is_object(), "{name}: missing expected profile");
        scalar_defaults(&mut actual);
        scalar_defaults(&mut expected);
        assert_eq!(actual, expected, "{name}: complete expanded profile");

        // Exercise the production custom-ePDG precedence with the same synthetic
        // IMSI substitution used by the independent proposal corpus.
        let mut tunnel_sim: SimIdentity = serde_json::from_value(row["sim"].clone()).unwrap();
        tunnel_sim.imsi = format!("{}{}1234567890", sim.mcc, sim.mnc);
        let selected = selected.unwrap_or_else(|| ResolvedCarrier {
            id: profile.id.clone(),
            bundle: String::new(),
            selector: String::new(),
            carrier_name: None,
            configuration: json!({}),
        });
        let (tunnel, _) = selected
            .native_session_tunnel_config(&tunnel_sim, None, "custom.epdg.example.invalid", None)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(row["custom_epdg"]["address"], tunnel.epdg, "{name}");
        assert_eq!(row["custom_epdg"]["source"], tunnel.epdg_source, "{name}");
    }
    assert_eq!(rejected, 3);
}
