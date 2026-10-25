//! Current YAML export reproducibility; historical independent Go replay is separate.
#[path = "../examples/carrier_export/support.rs"]
mod support;

use serde_json::Value;
use std::path::Path;

fn difference(actual: &Value, expected: &Value, path: &str) -> Option<String> {
    if actual == expected {
        return None;
    }
    match (actual, expected) {
        (Value::Object(a), Value::Object(b)) if a.keys().eq(b.keys()) => a
            .iter()
            .find_map(|(key, value)| difference(value, &b[key], &format!("{path}/{key}"))),
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .enumerate()
            .find_map(|(index, (a, b))| difference(a, b, &format!("{path}/{index}"))),
        _ => Some(path.to_owned()),
    }
}

#[test]
fn yaml_rebuild_matches_current_registry_and_case_goldens() {
    let data = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../data/carriers"
    ));
    let registry = support::rebuild_registry(data, support::PINNED_REVISION).unwrap();
    assert_eq!(registry["selectors"].as_object().unwrap().len(), 858);
    assert_eq!(registry["profiles"].as_object().unwrap().len(), 697);
    let expected: Value =
        serde_json::from_str(include_str!("../../../carrier-registry.json")).unwrap();
    assert_eq!(difference(&registry, &expected, "registry"), None);
    let repeated = support::rebuild_registry(data, support::PINNED_REVISION).unwrap();
    assert_eq!(
        support::encode(&registry).unwrap(),
        support::encode(&repeated).unwrap()
    );

    let cases = support::rebuild_selection_cases(&registry).unwrap();
    let rows = cases.as_array().unwrap();
    assert_eq!(rows.len(), 2811);
    assert_eq!(rows.iter().filter(|row| row["rejected"] == true).count(), 3);
    let expected: Value =
        serde_json::from_str(include_str!("../../../carrier-cases.json")).unwrap();
    assert_eq!(difference(&cases, &expected, "cases"), None);
    assert_eq!(
        support::encode(&cases).unwrap(),
        support::encode(&support::rebuild_selection_cases(&registry).unwrap()).unwrap()
    );
}
