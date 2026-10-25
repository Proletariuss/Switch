//! Native authoritative reader information projection; no compatibility RPC.
use crate::ServerState;
use proletarius_domain::BackendFailure;
use serde_json::{Map, Value};

struct ReaderInfoFacts {
    status: Value,
    usim_info: Option<Value>,
    esim_profile_name: String,
}

pub(crate) async fn capture(state: &ServerState) -> Result<Value, BackendFailure> {
    let service = state
        .reader_lifecycle()
        .map_err(|error| BackendFailure::Io(error.to_string()))?;
    let snapshot = service
        .observed_snapshot()
        .await
        .map_err(|error| BackendFailure::Io(error.to_string()))?;
    if snapshot.selection.pending {
        return Err(BackendFailure::Io(
            "reader selection changed during information observation".into(),
        ));
    }
    project_native(snapshot, state.multistandby_store.data_root())
}
pub(crate) fn project_native(
    snapshot: crate::reader_lifecycle::Snapshot,
    data_root: &std::path::Path,
) -> Result<Value, BackendFailure> {
    let mut info = snapshot.usim_info;
    if let Some(fields) = info.as_mut().and_then(Value::as_object_mut) {
        // Routing facts belong in the private atomic snapshot. Public output
        // preserves the existing Go usimInfo projection and optional fields.
        for key in ["smsc_psi", "smsc_target", "spn", "isim_aid"] {
            fields.remove(key);
        }
        for key in [
            "phone_number",
            "operator_name",
            "operator_source",
            "gid1",
            "gid2",
            "application_aid",
            "smsc",
            "private_identity",
            "public_identity",
            "home_domain",
        ] {
            if fields.get(key).and_then(Value::as_str) == Some("") {
                fields.remove(key);
            }
        }
    }
    let facts = ReaderInfoFacts {
        status: snapshot.status,
        usim_info: info,
        esim_profile_name: snapshot.esim_profile_name,
    };
    let fallback = phone_number_fallback(&facts, &crate::profile_notes::Store::new(data_root));
    project(facts, &fallback)
}

fn phone_number_fallback(facts: &ReaderInfoFacts, notes: &crate::profile_notes::Store) -> String {
    let Some(fields) = facts.usim_info.as_ref().and_then(Value::as_object) else {
        return String::new();
    };
    if !crate::profile_notes::is_missing_phone(string_field(fields, "phone_number")) {
        return String::new();
    }
    notes.phone_for_iccid_or_empty(string_field(fields, "iccid"))
}

fn project(facts: ReaderInfoFacts, phone_fallback: &str) -> Result<Value, BackendFailure> {
    let mut response = Map::new();
    response.insert("status".into(), facts.status);
    let Some(mut info) = facts.usim_info else {
        return Ok(Value::Object(response));
    };
    let Some(fields) = info.as_object_mut() else {
        return Err(BackendFailure::GoBackendUnavailable(
            "reader SIM information is malformed".into(),
        ));
    };

    if crate::profile_notes::is_missing_phone(string_field(fields, "phone_number")) {
        if phone_fallback.trim().is_empty() {
            fields.remove("phone_number");
        } else {
            fields.insert(
                "phone_number".into(),
                Value::String(phone_fallback.trim().to_owned()),
            );
        }
    }

    if string_field(fields, "operator_name").is_empty() {
        if !facts.esim_profile_name.trim().is_empty() {
            fields.insert(
                "operator_name".into(),
                Value::String(facts.esim_profile_name.trim().to_owned()),
            );
            fields.insert(
                "operator_source".into(),
                Value::String("esim_profile".into()),
            );
        } else {
            let mcc = string_field(fields, "mcc").trim().to_owned();
            let mnc = string_field(fields, "mnc").trim().to_owned();
            let operator = format!("{mcc}/{mnc}");
            fields.insert(
                "operator_name".into(),
                Value::String(operator.trim_matches('/').to_owned()),
            );
            fields.insert("operator_source".into(), Value::String("mcc_mnc".into()));
        }
    }

    let sim = proletarius_vowifi::carrier::SimIdentity {
        mcc: string_field(fields, "mcc").to_owned(),
        mnc: string_field(fields, "mnc").to_owned(),
        imsi: string_field(fields, "imsi").to_owned(),
        iccid: string_field(fields, "iccid").to_owned(),
        gid1: string_field(fields, "gid1").to_owned(),
        gid2: string_field(fields, "gid2").to_owned(),
        spn: if string_field(fields, "operator_source") == "ef_spn" {
            string_field(fields, "operator_name").trim().to_owned()
        } else {
            String::new()
        },
    };
    match crate::carrier::resolve_epdg_fqdn(&sim) {
        Ok(epdg) if !epdg.is_empty() => {
            response.insert("epdg_fqdn".into(), Value::String(epdg));
        }
        Ok(_) => {}
        Err(error) => {
            response.insert("carrier_config_error".into(), Value::String(error));
        }
    }
    response.insert("usim_info".into(), info);
    Ok(Value::Object(response))
}

fn string_field<'a>(fields: &'a Map<String, Value>, key: &str) -> &'a str {
    fields.get(key).and_then(Value::as_str).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn facts() -> ReaderInfoFacts {
        ReaderInfoFacts {
            status: json!({"connected":true,"reader_name":"Fixture"}),
            usim_info: Some(
                json!({"iccid":"8986000000000000000","imsi":"454030000000000","mcc":"454","mnc":"03","operator_name":"","phone_number":""}),
            ),
            esim_profile_name: "SIMworld".into(),
        }
    }
    #[test]
    fn native_projection_preserves_carrier_profile_and_phone_fallback() {
        let value = project(facts(), "+85291234567").unwrap();
        assert_eq!(value["usim_info"]["phone_number"], "+85291234567");
        assert_eq!(value["usim_info"]["operator_name"], "SIMworld");
        assert_eq!(value["usim_info"]["operator_source"], "esim_profile");
        assert_eq!(value["epdg_fqdn"], "wlan.three.com.hk");
    }
    #[test]
    fn existing_card_number_and_operator_are_not_overwritten() {
        let mut facts = facts();
        let info = facts.usim_info.as_mut().unwrap();
        info["phone_number"] = json!("+85290000000");
        info["operator_name"] = json!("Card Name");
        info["operator_source"] = json!("ef_spn");
        let value = project(facts, "+85291234567").unwrap();
        assert_eq!(value["usim_info"]["phone_number"], "+85290000000");
        assert_eq!(value["usim_info"]["operator_name"], "Card Name");
    }
    #[test]
    fn disconnected_projection_has_only_legacy_status() {
        let value = project(
            ReaderInfoFacts {
                status: json!({"connected":false,"has_card":false}),
                usim_info: None,
                esim_profile_name: String::new(),
            },
            "",
        )
        .unwrap();
        assert_eq!(
            value,
            json!({"status":{"connected":false,"has_card":false}})
        );
    }
}
