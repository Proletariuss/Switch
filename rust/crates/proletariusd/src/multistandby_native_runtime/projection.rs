use super::*;
impl Manager {
    pub(crate) fn snapshot(&self, card_id: &str) -> Value {
        let state = self.state.lock().unwrap();
        let (selected, facts, _) = self.resources.gate.native_reader_snapshot();
        let current = state.inventory.as_ref().is_some_and(|i| {
            !selected.pending && selected.reader == i.reader && selected.generation == i.generation
        });
        let actual_iccid = facts
            .as_ref()
            .and_then(|f| f.metadata.as_ref())
            .and_then(|m| m["iccid"].as_str());
        let mut profiles = Vec::new();
        if let Some(inventory) = &state.inventory {
            for p in &inventory.profiles {
                if !card_id.is_empty() && p.card_id != card_id {
                    continue;
                }
                let entry = state.entries.get(&p.profile_id);
                let snap = entry.and_then(|e| e.session.as_ref()).map(|s| s.snapshot());
                let live = entry.is_some_and(|e| {
                    !*e.stop.borrow()
                        && !e.retry_requested
                        && !e.registration_pending
                        && e.attempt_stop.as_ref().is_none_or(|stop| !*stop.borrow())
                }) && current;
                let ready = live
                    && snap.as_ref().is_some_and(|s| {
                        s.termination.is_none()
                            && s.sms_ready
                            && s.sms_route_ready
                            && s.registered
                            && s.tunnel_ready
                    });
                let (tunnel, ims) = snap
                    .as_ref()
                    .map(|s| {
                        if s.termination.is_some() {
                            ("disconnected", "failed")
                        } else {
                            (
                                if s.tunnel_ready {
                                    "connected"
                                } else if s.tunnel_started {
                                    "connecting"
                                } else {
                                    "disconnected"
                                },
                                if s.registered {
                                    "registered"
                                } else if s.register_started {
                                    "registering"
                                } else {
                                    "idle"
                                },
                            )
                        }
                    })
                    .unwrap_or(("disconnected", "idle"));
                let mut error = entry.map(|e| e.error.clone()).unwrap_or_default();
                if let Some(s) = &snap {
                    if let Some(t) = &s.termination {
                        error = t
                            .startup_failure
                            .as_ref()
                            .map(|d| d.message)
                            .or(t.public_error)
                            .unwrap_or("native_multi_session_failed")
                            .into();
                    } else if let Some(warning) = s.registration_identity_error {
                        error = warning.into();
                    }
                }
                if error.is_empty()
                    && live
                    && let (Some(s), Some(e), Some(inventory)) = (&snap, entry, &state.inventory)
                    && s.binding.reader == inventory.reader
                    && s.binding.reader_generation == inventory.generation
                    && s.binding.session_generation.value() == e.generation
                    && let Some(warning) = s.active_sms_archive_warning()
                {
                    error = warning.message().into();
                }
                let phone = snap
                    .as_ref()
                    .and_then(|s| {
                        s.registered_msisdn
                            .as_ref()
                            .filter(|v| !crate::profile_notes::is_missing_phone(v))
                            .or(s.sim_phone_number.as_ref())
                    })
                    .filter(|v| !v.is_empty())
                    .cloned()
                    .unwrap_or_else(|| {
                        entry
                            .map(|e| e.spec.phone_number.clone())
                            .unwrap_or_else(|| p.phone_number.clone())
                    });
                let mut row = json!({"profile_id":p.profile_id,"card_id":p.card_id,"eid":p.eid,"iccid":p.iccid,"display_name":p.display_name,"phone_number":phone,"physical_enabled":physical_enabled(current, state.runtime.is_some(), actual_iccid, p),"desired_state":if state.desired.get(&p.profile_id).copied().unwrap_or(false){"keep_online"}else{"stopped"},"auth_state":if ready{"ready"}else{entry.map(|e|e.auth).unwrap_or("idle")},"tunnel_state":if current{tunnel}else{"disconnected"},"ims_state":if current{ims}else{"idle"},"sms_ready":ready,"health":if ready{"ready"}else{entry.map(|e|e.health).unwrap_or("stopped")},"last_error":error,"session_generation":entry.map(|e|e.generation).unwrap_or(0),"consecutive_failures":entry.map(|e|e.failures).unwrap_or(0)});
                if let Some(e) = entry {
                    if !e.reason.is_empty() {
                        row["recovery_reason"] = json!(e.reason)
                    }
                    if let Some((_, _, Some(due))) = &e.retry {
                        row["next_retry_at_ms"] = json!(due)
                    }
                }
                if let Some(s) = snap {
                    if let Some(time) = s.registration_expires_at_ms {
                        row["registration_expires_at_ms"] = json!(time)
                    }
                    if let Some(time) = s.registration_renew_at_ms {
                        row["registration_renew_at_ms"] = json!(time)
                    }
                }
                profiles.push(row);
            }
        }
        profiles.sort_by(|a, b| a["profile_id"].as_str().cmp(&b["profile_id"].as_str()));
        json!({"profiles":profiles,"boot_id":self.boot_id})
    }
}
fn normalize(value: &str) -> String {
    value.trim().trim_end_matches(['f', 'F']).to_owned()
}

fn physical_enabled(
    current: bool,
    active: bool,
    actual: Option<&str>,
    profile: &ProfileSpec,
) -> bool {
    current
        && match actual {
            Some(iccid) => normalize(iccid) == normalize(&profile.iccid),
            None => !active && profile.physical_enabled,
        }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_state_is_used_only_before_a_live_pool_can_change_the_card() {
        let profile = ProfileSpec {
            profile_id: "profile".into(),
            card_id: "card".into(),
            eid: "eid".into(),
            iccid: "1234".into(),
            aid: "aid".into(),
            display_name: String::new(),
            phone_number: String::new(),
            physical_enabled: true,
        };
        assert!(physical_enabled(true, false, None, &profile));
        assert!(!physical_enabled(true, true, None, &profile));
        assert!(!physical_enabled(false, false, None, &profile));
        assert!(!physical_enabled(true, false, Some("5678"), &profile));
        assert!(physical_enabled(true, true, Some("1234FF"), &profile));
    }
}
