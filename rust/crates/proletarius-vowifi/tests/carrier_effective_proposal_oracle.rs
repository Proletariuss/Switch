//! Frozen independent Go session config/encoder expectations; no Go runtime required.
use proletarius_ike::{ProposalProtocol, build_native_proposals};
use proletarius_vowifi::carrier::{Registry, ResolvedCarrier, SimIdentity};
use proletarius_vowifi::carrier_overrides::OverrideSet;
use serde_json::{Value, json};
#[test]
fn actual_go_effective_offers_match_native_for_all_registry_cases() {
    let bytes = std::env::var("SWITCH_PROPOSAL_ORACLE")
        .map(|path| std::fs::read(path).expect("read explicit proposal oracle"))
        .unwrap_or_else(|_| {
            include_bytes!("../../../testdata/retired-go-oracles/effective-proposals.json").to_vec()
        });
    let rows: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(rows.len(), 2730, "complete frozen proposal corpus");
    assert!(
        rows.iter().all(|row| row.get("error").is_none()),
        "success corpus must not silently acquire error rows"
    );
    let registry: Registry = serde_json::from_str(include_str!(
        "../../../testdata/carrier-history/carrier-registry.json"
    ))
    .unwrap();
    assert_eq!(
        registry.revision,
        "cdba73e551810681f16b4cce0693c70e9e216c37"
    );
    let mut failures = Vec::new();
    let mut checked = 0;
    for row in &rows {
        let mut input = serde_json::Map::new();
        for (key, value) in row["sim"].as_object().unwrap() {
            input.insert(key.to_ascii_lowercase(), value.clone());
        }
        let sim: SimIdentity = serde_json::from_value(Value::Object(input)).unwrap();
        let carrier = registry
            .resolve(&sim)
            .unwrap()
            .unwrap_or_else(|| ResolvedCarrier {
                id: format!("auto-{}-{}", sim.mcc, sim.mnc),
                bundle: String::new(),
                selector: String::new(),
                carrier_name: None,
                configuration: json!({}),
            });
        assert_eq!(row["id"], carrier.id);
        let yaml = row["yaml"]
            .as_str()
            .filter(|v| !v.is_empty())
            .unwrap_or("carrier_overrides: {}");
        let overrides = OverrideSet::parse(yaml.as_bytes()).unwrap();
        let (config, _) = match carrier.native_session_tunnel_config(
            &sim,
            overrides.get(&sim.mcc, &sim.mnc),
            "",
            None,
        ) {
            Ok(value) => value,
            Err(error) => {
                failures.push(json!({"id":carrier.id,"sim":row["sim"],"error":error}));
                continue;
            }
        };
        for (name, list, protocol, spi) in [
            ("ike", &config.proposals.ike, ProposalProtocol::Ike, &[][..]),
            (
                "esp",
                &config.proposals.esp,
                ProposalProtocol::Esp,
                &[0, 0, 0, 1][..],
            ),
        ] {
            let actual = build_native_proposals(list, protocol, spi).unwrap();
            let expected = row[name].as_str().unwrap();
            let expected = (0..expected.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&expected[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>();
            if actual != expected {
                failures.push(json!({"id":carrier.id,"sim":row["sim"],"error":format!("{name} effective wire differs"),"native":list}));
            }
        }
        checked += 1;
    }
    if let Ok(path) = std::env::var("SWITCH_PROPOSAL_SUMMARY") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(
                &json!({"rows":rows.len(),"native_session_compiled":checked,"failures":failures}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    assert!(
        failures.is_empty(),
        "{} proposal/preflight differences; first: {:?}",
        failures.len(),
        failures.first()
    );
    assert_eq!(checked, 2730, "every frozen proposal row must be consumed");
}
