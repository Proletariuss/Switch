use super::*;
pub(super) async fn handle(state: &ServerState) -> Response<Full<Bytes>> {
    let state = state.clone();
    let job = tokio::spawn(async move { read(&state).await });
    match tokio::time::timeout(SNAPSHOT_TIMEOUT, job).await {
        Ok(Ok(response)) => response,
        _ => project_error("esim_health_unavailable"),
    }
}
fn busy(state: &ServerState) -> Option<Response<Full<Bytes>>> {
    if state.module_radio.vowifi_owns_sim_channel() {
        Some(project_error("module_vowifi_owns_sim_channel"))
    } else if state.module_radio.sim_channel_busy()
        || crate::reader_esim_download::has_active_operation()
    {
        Some(profile_error("esim_health_failed"))
    } else {
        None
    }
}
fn profile_error(result: &str) -> Response<Full<Bytes>> {
    project_profile_result(&Snapshot {
        result: result.into(),
        profiles: vec![],
        operation: None,
    })
    .unwrap()
}
async fn read(state: &ServerState) -> Response<Full<Bytes>> {
    if let Some(response) = busy(state) {
        return response;
    }
    let _single = crate::module_esim_transport::snapshot_guard(state).await;
    if let Some(response) = busy(state) {
        return response;
    }
    let reservation = match state.module_radio.reserve_esim().await {
        Ok(value) => value,
        Err(error) => return busy(state).unwrap_or_else(|| error.module_radio_compat_response()),
    };
    let (response, key, invalidate) = observe(state).await;
    match state.module_radio.release_esim(reservation).await {
        Ok(())
            if key
                .as_ref()
                .is_none_or(|key| crate::module_esim_transport::snapshot_current(state, key)) =>
        {
            if invalidate {
                crate::module_esim_transport::invalidate(state);
            }
            response
        }
        Ok(()) => project_error("modem_status_failed"),
        Err(error) => error.module_radio_compat_response(),
    }
}
async fn observe(
    state: &ServerState,
) -> (
    Response<Full<Bytes>>,
    Option<crate::module_esim_transport::SnapshotKey>,
    bool,
) {
    let (key, overview) = match crate::module_esim_transport::overview(state, false).await {
        Ok(value) => value,
        Err(error) => {
            return (
                profile_error(if error.public_code() == "euicc_not_accessible" {
                    "euicc_not_accessible"
                } else {
                    "esim_health_failed"
                }),
                None,
                false,
            );
        }
    };
    let mut invalidate = false;
    let response = observe_profiles(state, &key, overview, &mut invalidate).await;
    (response, Some(key), invalidate)
}
async fn observe_profiles(
    state: &ServerState,
    key: &crate::module_esim_transport::SnapshotKey,
    overview: Value,
    invalidate: &mut bool,
) -> Response<Full<Bytes>> {
    let groups = match serde_json::from_value(overview["profiles"].clone()) {
        Ok(groups) => groups,
        Err(_) => return profile_error("esim_health_failed"),
    };
    let profiles = Snapshot {
        result: "profiles_observed".into(),
        profiles: groups,
        operation: crate::reader_esim_download::current_operation_json(),
    };
    if let Some(response) = project_profile_result(&profiles) {
        return response;
    }
    let active = find_active_profile(&profiles.profiles).unwrap();
    let status = match crate::module_esim_transport::health_status(state, key).await {
        Ok(value) => match serde_json::from_value::<ModuleStatus>(value) {
            Ok(status) => status,
            Err(_) => return project_error("modem_status_failed"),
        },
        Err(error) => {
            return project_error(
                if matches!(
                    error.public_code(),
                    "usb_at_not_found" | "usb_at_disconnected"
                ) {
                    "modem_unavailable"
                } else {
                    "modem_status_failed"
                },
            );
        }
    };
    if !crate::module_esim_transport::snapshot_current(state, key) {
        return project_error("modem_status_failed");
    }
    if !status.iccid.trim().is_empty()
        && normalize_sim_identifier(profile_iccid(active))
            != normalize_sim_identifier(&status.iccid)
    {
        *invalidate = true;
    }
    project_status(active, &status, profiles.operation)
}
