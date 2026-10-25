use super::*;
impl Service {
    pub(super) async fn refresh_inventory(self: &Arc<Self>) -> Result<(), Failure> {
        let selection = self.gate.selected_reader_generation();
        let old = self.cache.lock().unwrap().inventory.clone();
        if let Some(old) = old.as_ref()
            && selection != Some((old.reader.clone(), old.generation))
        {
            self.manager
                .reader_lost(old.reader.clone(), old.generation)?
                .wait()
                .await?;
            self.cache.lock().unwrap().recovered = None;
        }
        let (reader, generation) =
            selection.ok_or_else(|| Failure::from("native_multi_reader_unavailable"))?;
        if self.manager.has_active_runtime() {
            return if self
                .manager
                .cached_inventory()
                .is_some_and(|i| i.reader == reader && i.generation == generation)
            {
                Ok(())
            } else {
                Err("native_multi_inventory_stale".into())
            };
        }
        let current = old
            .as_ref()
            .is_some_and(|i| i.reader == reader && i.generation == generation);
        if current
            && self
                .cache
                .lock()
                .unwrap()
                .read_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(30))
        {
            return Ok(());
        }
        let raw = crate::multistandby_native_inventory::read(
            self.owner.clone(),
            self.gate.clone(),
            self.card.clone(),
            reader.clone(),
            generation,
        )
        .await
        .map_err(|error| Failure {
            code: error.code,
            message: error.message,
        })?;
        let mut profiles = parse(&raw)?;
        let catalog = json!({"reader":reader,"profiles":profiles.iter().map(|p|json!({
            "eid":p.eid,"iccid":p.iccid,"display_name":p.display_name,"phone_number":p.phone_number,
        })).collect::<Vec<_>>()});
        let unchanged = self.cache.lock().unwrap().catalog.as_ref() == Some(&catalog);
        let identities = if unchanged {
            let old = old
                .as_ref()
                .ok_or_else(|| Failure::from("native_multi_inventory_invalid"))?;
            json!(old.profiles.iter().map(|p|json!({"eid":p.eid,"iccid":p.iccid,"profile_id":p.profile_id,"card_id":p.card_id})).collect::<Vec<_>>())
        } else {
            let store = self.store.clone();
            let mut input = catalog.clone();
            input["event_id"] = json!(
                crate::multistandby_store::random_token()
                    .map_err(|_| Failure::from("native_multi_store_unavailable"))?
            );
            crate::archive_io::run(move || store.apply("catalog", input))
                .await
                .map_err(|_| Failure::from("native_multi_store_unavailable"))?["profiles"]
                .clone()
        };
        merge_identities(&mut profiles, &identities)?;
        let inventory = Inventory {
            reader,
            generation,
            profiles,
        };
        // Exact selection is checked again by the manager after durable commit;
        // no stale card enumeration can authorize startup on a newer reader.
        self.manager.install_inventory(inventory.clone()).await?;
        let mut cache = self.cache.lock().unwrap();
        if !current {
            cache.recovered = None;
        }
        // Keep unavailable-card discovery rows after a reader retirement, as
        // the previous public projection did, without retaining startup authority.
        cache.card_profiles = inventory
            .profiles
            .iter()
            .map(|p| json!({"card_id":p.card_id,"reader":inventory.reader,"eid":p.eid}))
            .collect();
        cache.inventory = Some(inventory);
        cache.catalog = Some(catalog);
        cache.read_at = Some(Instant::now());
        Ok(())
    }
    pub(super) async fn recover_once(self: &Arc<Self>) -> Result<(), Failure> {
        let inventory = self
            .cache
            .lock()
            .unwrap()
            .inventory
            .clone()
            .ok_or_else(|| Failure::from("native_multi_inventory_invalid"))?;
        let key = (inventory.reader.clone(), inventory.generation);
        if self.cache.lock().unwrap().recovered.as_ref() == Some(&key) {
            return Ok(());
        }
        if self.gate.selected_reader_generation() != Some(key.clone()) {
            return Err("native_multi_inventory_stale".into());
        }
        let ids = inventory
            .profiles
            .iter()
            .map(|p| p.profile_id.clone())
            .collect::<Vec<_>>();
        let store = self.store.clone();
        let selection = crate::archive_io::run(move || store.recovery_selection(&ids))
            .await
            .map_err(|_| Failure::from("native_multi_store_unavailable"))?;
        let (card, selected) = recovery(&inventory, &selection)?;
        // A failure/unknown return never causes repeated automatic dispatch.
        // A later manual operation remains separately claimed by its owner.
        self.cache.lock().unwrap().recovered = Some(key);
        if !selected.is_empty() {
            // Inventory availability describes discovery, not registration.
            // The former owner dispatched recovery once in a retained task;
            // an execution failure did not turn the valid inventory unavailable.
            if let Ok(ticket) = self.manager.execute(Action::Start, card, selected) {
                let _ = ticket.wait().await;
            }
        }
        Ok(())
    }
}
fn field(value: &Value, name: &str, maximum: usize) -> Result<String, Failure> {
    let text = value[name]
        .as_str()
        .ok_or_else(|| Failure::from("native_multi_inventory_invalid"))?;
    if text.len() > maximum || text.chars().any(char::is_control) {
        return Err("native_multi_inventory_invalid".into());
    }
    Ok(text.to_owned())
}
fn parse(raw: &Value) -> Result<Vec<ProfileSpec>, Failure> {
    let groups = raw["groups"]
        .as_array()
        .ok_or_else(|| Failure::from("native_multi_inventory_invalid"))?;
    let mut profiles = Vec::new();
    for group in groups {
        let eid = field(group, "eid", 128)?;
        if eid.trim().is_empty() {
            continue;
        }
        let aid = field(group, "aid_hex", 128)?;
        if aid.is_empty()
            || aid.len() % 2 != 0
            || !aid.bytes().all(|c| c.is_ascii_hexdigit())
            || group["read_state"] != "ready"
        {
            return Err("native_multi_inventory_invalid".into());
        }
        let items = group["profiles"]
            .as_array()
            .ok_or_else(|| Failure::from("native_multi_inventory_invalid"))?;
        for item in items {
            let iccid = field(item, "iccid", 128)?;
            if iccid.is_empty() {
                continue;
            }
            let mut name = field(item, "name", 512)?;
            if name.is_empty() {
                name = field(item, "service_provider_name", 512)?;
            }
            profiles.push(ProfileSpec {
                profile_id: String::new(),
                card_id: String::new(),
                eid: eid.clone(),
                iccid,
                aid: aid.clone(),
                display_name: name,
                phone_number: String::new(),
                physical_enabled: item["state"] == 1,
            });
            if profiles.len() > 512 {
                return Err("native_multi_inventory_invalid".into());
            }
        }
    }
    profiles.sort_by(|a, b| (&a.eid, &a.iccid).cmp(&(&b.eid, &b.iccid)));
    Ok(profiles)
}
fn merge_identities(profiles: &mut [ProfileSpec], identities: &Value) -> Result<(), Failure> {
    let values = identities
        .as_array()
        .ok_or_else(|| Failure::from("native_multi_inventory_invalid"))?;
    if values.len() != profiles.len() {
        return Err("native_multi_inventory_invalid".into());
    }
    let mut ids = std::collections::HashMap::new();
    for value in values {
        let eid = field(value, "eid", 128)?;
        let iccid = field(value, "iccid", 128)?;
        let profile = field(value, "profile_id", 128)?;
        let card = field(value, "card_id", 128)?;
        if eid.is_empty()
            || iccid.is_empty()
            || profile.is_empty()
            || card.is_empty()
            || ids.insert((eid, iccid), (profile, card)).is_some()
        {
            return Err("native_multi_inventory_invalid".into());
        }
    }
    for profile in profiles {
        let (id, card) = ids
            .remove(&(profile.eid.clone(), profile.iccid.clone()))
            .ok_or_else(|| Failure::from("native_multi_inventory_invalid"))?;
        profile.profile_id = id;
        profile.card_id = card;
    }
    Ok(())
}
fn recovery(inventory: &Inventory, selection: &Value) -> Result<(String, Vec<String>), Failure> {
    let card = field(selection, "card_id", 128)?;
    let values = selection["profile_ids"]
        .as_array()
        .ok_or_else(|| Failure::from("native_multi_inventory_invalid"))?;
    if values.is_empty() != card.is_empty() {
        return Err("native_multi_inventory_invalid".into());
    }
    let mut ids = Vec::new();
    for value in values {
        let id = value
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Failure::from("native_multi_inventory_invalid"))?;
        if ids.last().is_some_and(|last: &String| last.as_str() >= id)
            || !inventory
                .profiles
                .iter()
                .any(|p| p.profile_id == id && p.card_id == card)
        {
            return Err("native_multi_inventory_invalid".into());
        }
        ids.push(id.to_owned());
    }
    Ok((card, ids))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> Value {
        json!({"groups":[{"eid":"eid-A","aid_hex":"A00001","read_state":"ready","profiles":[
            {"iccid":"02","name":"","service_provider_name":"Carrier","state":0},
            {"iccid":"01","name":"Nickname","service_provider_name":"Carrier","state":1}
        ]}]})
    }
    #[test]
    fn decoding_preserves_identity_name_enabled_and_rejects_failed_groups() {
        let mut profiles = parse(&source()).unwrap();
        assert_eq!(profiles[0].iccid, "01");
        assert_eq!(profiles[0].display_name, "Nickname");
        assert!(profiles[0].physical_enabled);
        assert_eq!(profiles[1].display_name, "Carrier");
        let identities = json!([{"eid":"eid-A","iccid":"02","profile_id":"p2","card_id":"c"},
            {"eid":"eid-A","iccid":"01","profile_id":"p1","card_id":"c"}]);
        merge_identities(&mut profiles, &identities).unwrap();
        assert_eq!(profiles[0].profile_id, "p1");
        let mut bad = source();
        bad["groups"][0]["read_state"] = json!("error");
        assert!(parse(&bad).is_err());
        let mut duplicate = identities.clone();
        duplicate[0] = duplicate[1].clone();
        assert!(merge_identities(&mut profiles, &duplicate).is_err());
    }
    #[test]
    fn recovery_accepts_only_sorted_exact_live_profiles_on_one_selected_card() {
        let mut profiles = parse(&source()).unwrap();
        merge_identities(
            &mut profiles,
            &json!([{"eid":"eid-A","iccid":"01","profile_id":"p1","card_id":"c"},
            {"eid":"eid-A","iccid":"02","profile_id":"p2","card_id":"c"}]),
        )
        .unwrap();
        let inventory = Inventory {
            reader: "r".into(),
            generation: 1,
            profiles,
        };
        assert_eq!(
            recovery(
                &inventory,
                &json!({"card_id":"c","profile_ids":["p1","p2"]})
            )
            .unwrap()
            .1
            .len(),
            2
        );
        for invalid in [
            json!({"card_id":"other","profile_ids":["p1"]}),
            json!({"card_id":"c","profile_ids":["p2","p1"]}),
            json!({"card_id":"c","profile_ids":["p1","p1"]}),
            json!({"card_id":"c","profile_ids":[]}),
            json!({"card_id":"c","profile_ids":["missing"]}),
        ] {
            assert!(recovery(&inventory, &invalid).is_err());
        }
    }
}
