//! Offline source-data conversion shared by the example and its golden test.
use proletarius_vowifi::carrier::{Registry, SimIdentity};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::Path;

pub const PINNED_REVISION: &str = "09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25";

/// Preserve every YAML document field, including unknown extension fields.
/// This deliberately does not read the checked-in generated registry JSON.
pub fn rebuild_registry(data_dir: &Path, revision: &str) -> Result<Value, String> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("carrier revision must be a 40-character hexadecimal commit".into());
    }
    let mut collections = BTreeMap::new();
    for kind in ["selectors", "profiles"] {
        let directory = data_dir.join(kind);
        let mut documents = BTreeMap::new();
        for entry in std::fs::read_dir(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?
        {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("yaml")
            {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| format!("non-UTF8 carrier filename: {}", path.display()))?;
            let bytes =
                std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            let document: Value = serde_yaml::from_slice(&bytes)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if !document.is_object() {
                return Err(format!(
                    "{}: carrier document must be an object",
                    path.display()
                ));
            }
            if documents.insert(name.to_owned(), document).is_some() {
                return Err(format!("duplicate carrier document {kind}/{name}"));
            }
        }
        if documents.is_empty() {
            return Err(format!("{}: no YAML documents", directory.display()));
        }
        collections.insert(kind, documents);
    }
    let mut document = json!({
        "schema_version": 1,
        "revision": revision,
        "selectors": collections["selectors"],
        "profiles": collections["profiles"],
    });
    let overlay_path = data_dir.join("compatibility/registry-overlay.json");
    if overlay_path.exists() {
        let overlay: Value = serde_json::from_slice(
            &std::fs::read(&overlay_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("{}: {error}", overlay_path.display()))?;
        apply_compatibility(&mut document, &overlay, revision)?;
    }
    Ok(document)
}

/// A separately attributed legacy corpus retains only explicitly reviewed
/// selector identities. A future upstream refresh must review this overlay.
fn apply_compatibility(
    document: &mut Value,
    overlay: &Value,
    revision: &str,
) -> Result<(), String> {
    if overlay["schema_version"] != 1 || overlay["upstream_revision"] != revision {
        return Err(
            "carrier compatibility overlay requires review for this upstream revision".into(),
        );
    }
    let profiles = overlay["profiles"]
        .as_object()
        .ok_or("compatibility profiles must be objects")?;
    for (name, profile) in profiles {
        if !name.starts_with("legacy_")
            || !profile.is_object()
            || document["profiles"].get(name).is_some()
        {
            return Err(format!("invalid or colliding compatibility profile {name}"));
        }
        document["profiles"]
            .as_object_mut()
            .unwrap()
            .insert(name.clone(), profile.clone());
    }
    for (plmn, manifest) in overlay["selectors"]
        .as_object()
        .ok_or("compatibility selectors must be objects")?
    {
        if manifest["plmn"] != *plmn {
            return Err(format!("compatibility PLMN mismatch: {plmn}"));
        }
        let retained = manifest["profiles"]
            .as_array()
            .ok_or("compatibility candidates must be arrays")?;
        let mut raw = std::collections::BTreeSet::new();
        for candidate in retained {
            let path = candidate["path"]
                .as_str()
                .ok_or("missing compatibility profile path")?;
            let name = path
                .strip_prefix("profiles/")
                .and_then(|v| v.strip_suffix(".yaml"))
                .ok_or("invalid compatibility profile path")?;
            if name.contains('/')
                || !profiles.contains_key(name)
                || profiles[name]["id"] != candidate["id"]
            {
                return Err(format!("compatibility profile identity mismatch: {path}"));
            }
            for selector in candidate["selectors"]
                .as_array()
                .ok_or("missing compatibility selector")?
            {
                let key = selector["raw"]
                    .as_str()
                    .ok_or("invalid compatibility selector key")?;
                if !raw.insert(key.to_owned()) {
                    return Err(format!("duplicate compatibility selector {key}"));
                }
            }
        }
        let selectors = document["selectors"].as_object_mut().unwrap();
        let target = selectors.entry(plmn.clone()).or_insert_with(|| {
            json!({
                "version": 2, "plmn": plmn, "profiles": []
            })
        });
        let candidates = target["profiles"]
            .as_array_mut()
            .ok_or("upstream candidates must be arrays")?;
        for candidate in candidates.iter_mut() {
            let rules = candidate["selectors"]
                .as_array_mut()
                .ok_or("upstream selectors must be arrays")?;
            rules.retain(|selector| {
                !selector["raw"]
                    .as_str()
                    .is_some_and(|key| raw.contains(key))
            });
            if raw.contains(plmn) {
                candidate["generic"] = json!(false);
            }
        }
        candidates.retain(|candidate| !candidate["selectors"].as_array().unwrap().is_empty());
        candidates.extend(retained.iter().cloned());
    }
    document["compatibility"] = json!({
        "source_revision": overlay["source_revision"],
        "source_registry_sha256": overlay["source_registry_sha256"],
        "reason": overlay["reason"]
    });
    Ok(())
}

#[derive(Deserialize)]
struct Manifest {
    plmn: String,
    #[serde(default)]
    profiles: Vec<Candidate>,
}
#[derive(Deserialize)]
struct Candidate {
    #[serde(default)]
    selectors: Vec<Selector>,
}
#[derive(Deserialize)]
struct Selector {
    #[serde(default)]
    conditions: Vec<Condition>,
}
#[derive(Deserialize)]
struct Condition {
    field: String,
    value: String,
}

/// Derive synthetic inputs from sorted selector documents and resolve against
/// the newly rebuilt registry. The committed case file is only a test golden.
pub fn rebuild_selection_cases(document: &Value) -> Result<Value, String> {
    let registry: Registry = serde_json::from_value(document.clone())
        .map_err(|error| format!("rebuilt registry is invalid: {error}"))?;
    let selectors = document["selectors"]
        .as_object()
        .ok_or("rebuilt registry has no selectors")?;
    let mut names: Vec<_> = selectors.keys().collect();
    names.sort();
    let mut cases = Vec::new();
    for name in names {
        let manifest: Manifest = serde_json::from_value(selectors[name].clone())
            .map_err(|error| format!("selector {name}: {error}"))?;
        if !(5..=6).contains(&manifest.plmn.len())
            || !manifest.plmn.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(format!("selector {name}: invalid numeric PLMN"));
        }
        let (mcc, mnc) = manifest.plmn.split_at(3);
        let mut append_case = |mut values: Map<String, Value>| -> Result<(), String> {
            values.insert("mcc".into(), json!(mcc));
            values.insert("mnc".into(), json!(mnc));
            let sim: SimIdentity = serde_json::from_value(Value::Object(values.clone()))
                .map_err(|error| format!("selector {name}: {error}"))?;
            let (id, selector, rejected) = match registry.resolve(&sim) {
                Ok(Some(profile)) => (profile.id, profile.selector, false),
                Ok(None) => (String::new(), String::new(), false),
                Err(_) => (String::new(), String::new(), true),
            };
            cases
                .push(json!({"sim": values, "id": id, "selector": selector, "rejected": rejected}));
            Ok(())
        };
        append_case(Map::new())?;
        for profile in manifest.profiles {
            for selector in profile.selectors {
                let mut values = Map::new();
                for condition in selector.conditions {
                    values.insert(condition.field.to_lowercase(), json!(condition.value));
                }
                append_case(values)?;
            }
        }
    }
    Ok(Value::Array(cases))
}

pub fn encode(document: &Value) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(document).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    Ok(bytes)
}
