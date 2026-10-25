use super::*;

pub(super) async fn handle(state: &ServerState, refresh: bool) -> Response<Full<Bytes>> {
    let state = state.clone();
    let intent_id = crate::snapshot::new_instance_id();
    let worker_id = intent_id.clone();
    let binding = refresh
        .then(|| crate::module_esim_transport::capture_refresh_binding(&state))
        .flatten();
    // Cancelled HTTP waiters cannot strand a reservation or cancel its physical
    // command. The worker owns through actual release, including past timeout.
    let progress = crate::esim_refresh::Progress::default();
    let worker_progress = progress.clone();
    let mut job = tokio::spawn(async move {
        if !refresh {
            return read(&state, false, &mut crate::esim_refresh::Evidence::default()).await;
        }
        let current_state = state.clone();
        let current_binding = binding.clone();
        crate::esim_refresh::run_with_id(
            &worker_id,
            "module",
            move || {
                worker_progress.started();
                let state = state.clone();
                let bound = binding.is_some();
                async move {
                    let mut evidence = crate::esim_refresh::Evidence::unknown(false);
                    let response = read(&state, true, &mut evidence).await;
                    if !bound {
                        evidence.preflight_transport = false;
                    }
                    crate::esim_refresh::Attempt { response, evidence }
                }
            },
            move || {
                current_binding.as_ref().is_none_or(|binding| {
                    crate::module_esim_transport::refresh_binding_current(&current_state, binding)
                })
            },
            tokio::time::sleep,
        )
        .await
    });
    if refresh {
        return crate::esim_refresh::wait(
            &intent_id,
            "module",
            &progress,
            &mut job,
            SNAPSHOT_TIMEOUT,
        )
        .await;
    }
    match tokio::time::timeout(SNAPSHOT_TIMEOUT, job).await {
        Ok(Ok(response)) => response,
        _ => coded_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_overview_unavailable",
            "读取 eSIM 卡片信息超时，模块事务仍在完成清理，请稍后重试",
        ),
    }
}
fn base(state: &ServerState) -> Snapshot {
    Snapshot {
        result: String::new(),
        euicc_unavailable: false,
        operation_in_progress: false,
        switch_allowed: true,
        modem_available: true,
        overview: None,
        cached: crate::module_esim_transport::cached_overview(state, true).map(|(_, value)| value),
        operation: crate::reader_esim_download::current_operation_json(),
    }
}
fn busy(state: &ServerState, refresh: bool) -> Option<Snapshot> {
    let mut snapshot = base(state);
    snapshot.result = if state.module_radio.vowifi_owns_sim_channel() {
        "module_vowifi_owns_radio"
    } else if refresh && crate::reader_esim_download::has_active_operation() {
        "refresh_operation_busy"
    } else if state.module_radio.sim_channel_busy()
        || crate::reader_esim_download::has_active_operation()
    {
        "module_channel_busy"
    } else {
        return None;
    }
    .into();
    Some(snapshot)
}
async fn read(
    state: &ServerState,
    refresh: bool,
    evidence: &mut crate::esim_refresh::Evidence,
) -> Response<Full<Bytes>> {
    *evidence = crate::esim_refresh::Evidence {
        cleanup_confirmed: true,
        ..Default::default()
    };
    if let Some(snapshot) = busy(state, refresh) {
        return project(&snapshot);
    }
    let _single = crate::module_esim_transport::snapshot_guard(state).await;
    if let Some(snapshot) = busy(state, refresh) {
        return project(&snapshot);
    }
    if !refresh
        && let Some((_, overview)) = crate::module_esim_transport::cached_overview(state, false)
    {
        let mut snapshot = base(state);
        snapshot.result = "ready".into();
        snapshot.overview = Some(overview);
        return project(&snapshot);
    }
    let reservation = match state.module_radio.reserve_esim().await {
        Ok(value) => value,
        Err(error) => {
            return busy(state, refresh).map_or_else(
                || error.module_radio_compat_response(),
                |snapshot| project(&snapshot),
            );
        }
    };
    let observation = crate::module_esim_transport::overview(state, refresh).await;
    if let Err(error) = record_release(evidence, state.module_radio.release_esim(reservation).await)
    {
        return error.module_radio_compat_response();
    }
    *evidence = crate::esim_refresh::Evidence::unknown(true);
    let mut snapshot = base(state);
    match observation {
        Ok((key, value)) if crate::module_esim_transport::snapshot_current(state, &key) => {
            evidence.succeeded = true;
            snapshot.result = "ready".into();
            snapshot.overview = Some(value);
        }
        Ok(_) => {
            snapshot.result = if refresh {
                "refresh_failed"
            } else {
                "overview_failed"
            }
            .into();
        }
        Err(error) => {
            if !error.uncertain && matches!(error.public_code(), "usb_at_busy" | "serial_card_busy")
            {
                // These codes originate before the physical transaction is
                // admitted. Generic I/O/timeouts never receive this evidence.
                *evidence = crate::esim_refresh::Evidence::preflight(true);
            }
            snapshot.result = if refresh {
                "refresh_failed"
            } else {
                "overview_failed"
            }
            .into();
            snapshot.euicc_unavailable = error.public_code() == "euicc_not_accessible";
            snapshot.operation_in_progress = matches!(
                error.public_code(),
                "usb_at_busy" | "usb_at_wait_timeout" | "serial_card_busy"
            );
        }
    }
    project(&snapshot)
}

fn record_release<E>(
    evidence: &mut crate::esim_refresh::Evidence,
    release: Result<(), E>,
) -> Result<(), E> {
    if release.is_err() {
        // A failed release means the physical owner has not proved cleanup.
        // Keep that distinction in the Rust receipt so neither this owner nor
        // a client can treat the attempt as an ordinary completed failure and
        // replay it.
        *evidence = crate::esim_refresh::Evidence::unknown(false);
    }
    release
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_release_adapter_marks_cleanup_unconfirmed() {
        let mut evidence = crate::esim_refresh::Evidence {
            cleanup_confirmed: true,
            ..Default::default()
        };
        let release: Result<(), &str> = Err("synthetic release failure");
        assert!(record_release(&mut evidence, release).is_err());
        assert!(!evidence.cleanup_confirmed);
        assert!(evidence.submitted_or_unknown);
        assert!(!evidence.preflight_transport);
        assert!(!evidence.succeeded);
    }
}
