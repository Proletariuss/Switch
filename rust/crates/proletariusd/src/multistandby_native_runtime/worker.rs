use super::*;
use crate::multistandby_runtime_report::native::Observation;
use crate::native_vowifi_session::multi::{EndReason, RetryClass};
impl Manager {
    pub(super) async fn run_profile(
        self: Arc<Self>,
        spec: ProfileSpec,
        inventory: Inventory,
        runtime: Arc<Runtime>,
        mut stop: watch::Receiver<bool>,
        mut wake: mpsc::Receiver<Wake>,
    ) -> Result<(), Failure> {
        loop {
            if *stop.borrow() {
                return Ok(());
            }
            let mut diagnostic = crate::safe_events::NwaitSpan::begin(self.diagnostic_hub());
            let (attempt_stop, mut cancelled) = watch::channel(false);
            {
                let mut state = self.state.lock().unwrap();
                let e = state
                    .entries
                    .get_mut(&spec.profile_id)
                    .ok_or("native_multi_profile_retired")?;
                e.attempt_stop = Some(attempt_stop.clone());
                e.retry_requested = false;
                e.retry = None;
                e.installing_retry = None;
                e.registration_pending = false;
                e.auth = "queued";
                e.health = "starting";
                e.reason = "starting".into();
            }
            // Serialize complete preparation/EAP/REGISTER, as the previous manager did.
            // Established profiles retain neither this permit nor a physical card pin.
            let startup = self.startup_gate.lock();
            tokio::pin!(startup);
            let acquired = tokio::select! {guard=&mut startup=>Some(guard),_=stop.changed()=>None,_=cancelled.changed()=>None};
            let Some(guard) = acquired else {
                if let Some(span) = diagnostic.as_mut() {
                    span.finish(
                        if *stop.borrow() {
                            "manual_stop"
                        } else {
                            "manual_retry"
                        },
                        true,
                    );
                }
                if *stop.borrow() {
                    return Ok(());
                }
                continue;
            };
            if *stop.borrow() {
                if let Some(span) = diagnostic.as_mut() {
                    span.finish("manual_stop", true);
                }
                return Ok(());
            }
            if *cancelled.borrow() {
                if let Some(span) = diagnostic.as_mut() {
                    span.finish("manual_retry", true);
                }
                continue;
            }
            self.set_phase(&spec.profile_id, "authenticating", "starting", None);
            let mut startup_guard = Some(guard);
            let options = &self.resources.connection;
            let start = runtime.begin_start_for_profile(
                Profile {
                    eid: spec.eid.clone(),
                    aid: spec.aid.clone(),
                    iccid: spec.iccid.clone(),
                },
                PlanRequest {
                    reader: inventory.reader.clone(),
                    proxy: options.proxy.clone(),
                    epdg: options.epdg.clone(),
                    epdg_port: options.epdg_port,
                },
                cancelled.clone(),
                self.resources.store.clone(),
                spec.profile_id.clone(),
            );
            let started = match start {
                Ok(start) => {
                    let wait = start.wait();
                    tokio::pin!(wait);
                    loop {
                        tokio::select! {result=&mut wait=>break result,changed=stop.changed()=>{if changed.is_err()||*stop.borrow(){attempt_stop.send_replace(true);}}}
                    }
                }
                Err(error) => Err(error),
            };
            let (mut reason, mut retry) = match started {
                Err(error) => {
                    self.update_error(&spec.profile_id, &error.code);
                    if !error.cleanup_confirmed {
                        return Err("native_multi_cleanup_unconfirmed");
                    }
                    ("startup_failed", error.retry)
                }
                Ok(session) => {
                    let session = Arc::new(session);
                    let generation = session.generation().value();
                    if let Some(span) = diagnostic.as_mut() {
                        span.connected(generation);
                    }
                    {
                        let mut state = self.state.lock().unwrap();
                        let e = state
                            .entries
                            .get_mut(&spec.profile_id)
                            .ok_or("native_multi_profile_retired")?;
                        e.session = Some(session.clone());
                        e.generation = generation;
                        e.error.clear();
                    }
                    let mut status = session.status_watch();
                    let mut forced = None;
                    loop {
                        let snap = session.snapshot();
                        if let Some(span) = diagnostic.as_mut() {
                            span.readiness(self.send_readiness(&spec.profile_id).channel_ready);
                        }
                        if snap.registered {
                            startup_guard.take();
                            self.set_phase(
                                &spec.profile_id,
                                "ready",
                                if snap.sms_ready { "ready" } else { "degraded" },
                                None,
                            );
                        }
                        let observation = Observation {
                            profile_id: spec.profile_id.clone(),
                            generation,
                            reason: if snap.sms_ready {
                                "registered"
                            } else {
                                "readiness"
                            }
                            .into(),
                            desired: !*stop.borrow(),
                            tunnel_connected: snap.tunnel_ready,
                            ims_registered: snap.registered,
                            messaging_ready: snap.sms_ready,
                            sim_sms_route_ready: snap.sms_route_ready,
                        };
                        if self.observe(observation).await.is_err() {
                            self.update_error(&spec.profile_id, "runtime_journal_unavailable");
                            forced = Some("backend_exit");
                            session.request_stop();
                            break;
                        }
                        tokio::select! {
                         _=session.wait_exit()=>break,
                         changed=stop.changed()=>{if changed.is_err()||*stop.borrow(){attempt_stop.send_replace(true);forced=Some("manual_stop");break}},
                         changed=status.changes()=>{if changed.is_err(){break}},
                         request=wake.recv()=>match request {
                          Some(Wake::Maintenance)=>{self.clear_maintenance(&spec.profile_id);if self.send_readiness(&spec.profile_id).channel_ready{if let Err(error)=self.flush(&spec.profile_id,&session).await{self.update_error(&spec.profile_id,error)}}},
                          Some(Wake::Retry) if self.retry_requested(&spec.profile_id)=>{forced=Some("manual_retry");session.request_stop();break},
                          Some(Wake::Registration(due)) if due==generation=>{forced=Some("planned_renewal");session.request_stop();break},
                          Some(_)=>{},None=>{forced=Some("manual_stop");session.request_stop();break}
                         }
                        }
                    }
                    let exit = session.stop_and_join().await;
                    if !exit.joined || !exit.cleanup_confirmed {
                        self.update_error(&spec.profile_id, "native_multi_cleanup_unconfirmed");
                        return Err("native_multi_cleanup_unconfirmed");
                    }
                    {
                        let mut state = self.state.lock().unwrap();
                        let e = state
                            .entries
                            .get_mut(&spec.profile_id)
                            .ok_or("native_multi_profile_retired")?;
                        e.session = None;
                        e.registration_pending = false;
                        if let Some(t) = &exit.snapshot.termination {
                            e.error = t
                                .startup_failure
                                .as_ref()
                                .map(|d| d.message)
                                .or(t.public_error)
                                .unwrap_or("")
                                .into();
                        }
                    }
                    let terminal = exit.snapshot.termination.as_ref();
                    let reason = forced.unwrap_or_else(|| {
                        terminal
                            .map(|t| match t.reason {
                                EndReason::RegistrationRenewal => "planned_renewal",
                                EndReason::PlannedReauthentication => "planned_ike_reauth",
                                EndReason::ReaderChanged => "reader_lost",
                                EndReason::StartupFailure | EndReason::SetupFailure => {
                                    "startup_failed"
                                }
                                EndReason::NetworkFailure => "tunnel_lost",
                                _ => "ims_lost",
                            })
                            .unwrap_or("ims_lost")
                    });
                    let retry = if forced.is_some() {
                        RetryClass::RetryAfterRelease
                    } else {
                        terminal.map(|t| t.retry).unwrap_or(RetryClass::Permanent)
                    };
                    let desired = !*stop.borrow();
                    let _ = self
                        .observe(Observation {
                            profile_id: spec.profile_id.clone(),
                            generation,
                            reason: if desired { reason } else { "manual_stop" }.into(),
                            desired,
                            tunnel_connected: false,
                            ims_registered: false,
                            messaging_ready: false,
                            sim_sms_route_ready: false,
                        })
                        .await;
                    (reason, retry)
                }
            };
            startup_guard.take();
            {
                let mut state = self.state.lock().unwrap();
                if let Some(e) = state.entries.get_mut(&spec.profile_id) {
                    e.attempt_stop = None;
                    e.maintenance_pending = false;
                }
            }
            if *stop.borrow() {
                if let Some(span) = diagnostic.as_mut() {
                    span.finish("manual_stop", true);
                }
                return Ok(());
            }
            if self.take_retry(&spec.profile_id) {
                reason = "manual_retry";
                retry = RetryClass::RetryAfterRelease;
            }
            // Cancellation can end the retained session before Wake::Retry is read.
            // Record the same resolved reason used by recovery after take_retry.
            if let Some(span) = diagnostic.as_mut() {
                span.finish(reason, true);
            }
            self.set_phase(&spec.profile_id, "backoff", "degraded", Some(reason));
            if self.resources.gate.selected_reader_generation()
                != Some((inventory.reader.clone(), inventory.generation))
                || reason == "reader_lost"
            {
                self.update_error(&spec.profile_id, "native_reader_changed");
                return Ok(());
            }
            if retry != RetryClass::RetryAfterRelease || reason == "backend_exit" {
                if !self
                    .wait_manual(&spec.profile_id, &mut stop, &mut wake)
                    .await
                {
                    return Ok(());
                }
                continue;
            }
            let journal = self.resources.journal.clone();
            let id = spec.profile_id.clone();
            let why = reason.to_owned();
            let owner = self.boot_id.clone();
            let attempt = retry_attempt()?;
            let scheduled_attempt = attempt.clone();
            {
                let mut state = self.state.lock().unwrap();
                let e = state
                    .entries
                    .get_mut(&spec.profile_id)
                    .ok_or("native_multi_profile_retired")?;
                e.installing_retry = Some(attempt.clone());
            }
            let planned = tokio::task::spawn_blocking(move || {
                let mut journal = journal.lock().map_err(|_| "runtime_journal_unavailable")?;
                let plan = journal.recovery_native(id.clone(), why, now())?;
                let schedule = journal.schedule_native_retry(
                    id,
                    owner,
                    scheduled_attempt,
                    plan.delay_ms,
                    now(),
                )?;
                Ok::<_, Failure>((plan, schedule))
            })
            .await
            .map_err(|_| "runtime_journal_unavailable")
            .and_then(|v| v);
            let (plan, schedule) = match planned {
                Ok(value) => value,
                Err(error) => {
                    if let Some(e) = self.state.lock().unwrap().entries.get_mut(&spec.profile_id) {
                        if e.installing_retry.as_deref() == Some(attempt.as_str()) {
                            e.installing_retry = None;
                        }
                    }
                    self.update_error(&spec.profile_id, error);
                    self.set_phase(
                        &spec.profile_id,
                        "backoff",
                        "degraded",
                        Some("backend_exit"),
                    );
                    if !self
                        .wait_manual(&spec.profile_id, &mut stop, &mut wake)
                        .await
                    {
                        return Ok(());
                    }
                    continue;
                }
            };
            {
                let mut state = self.state.lock().unwrap();
                let e = state
                    .entries
                    .get_mut(&spec.profile_id)
                    .ok_or("native_multi_profile_retired")?;
                e.reason = plan.reason;
                e.failures = plan.failures;
                e.retry = Some((attempt, schedule.generation, schedule.due_at_ms));
                e.installing_retry = None;
            }
            if plan.delay_ms == 0 {
                continue;
            }
            loop {
                if self.take_retry(&spec.profile_id) {
                    break;
                }
                tokio::select! {changed=stop.changed()=>{if changed.is_err()||*stop.borrow(){return Ok(())}},request=wake.recv()=>match request{Some(Wake::RetryDue(generation)) if generation==schedule.generation=>break,Some(Wake::Maintenance)=>self.clear_maintenance(&spec.profile_id),Some(_)=>{},None=>return Ok(())}}
            }
        }
    }
    fn retry_requested(&self, id: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .entries
            .get(id)
            .is_some_and(|e| e.retry_requested)
    }
    fn take_retry(&self, id: &str) -> bool {
        let mut s = self.state.lock().unwrap();
        s.entries.get_mut(id).is_some_and(|e| {
            let wanted = e.retry_requested;
            e.retry_requested = false;
            if wanted {
                e.retry = None;
            }
            wanted
        })
    }
    fn clear_maintenance(&self, id: &str) {
        if let Some(e) = self.state.lock().unwrap().entries.get_mut(id) {
            e.maintenance_pending = false;
        }
    }
    fn set_phase(&self, id: &str, auth: &'static str, health: &'static str, reason: Option<&str>) {
        if let Some(e) = self.state.lock().unwrap().entries.get_mut(id) {
            e.auth = auth;
            e.health = health;
            if let Some(reason) = reason {
                e.reason = reason.into();
            }
        }
    }
    fn update_error(&self, id: &str, error: &str) {
        if let Some(e) = self.state.lock().unwrap().entries.get_mut(id) {
            e.error = error.to_owned();
        }
    }
    async fn wait_manual(
        &self,
        id: &str,
        stop: &mut watch::Receiver<bool>,
        wake: &mut mpsc::Receiver<Wake>,
    ) -> bool {
        loop {
            if *stop.borrow() {
                return false;
            }
            if self.take_retry(id) {
                return true;
            }
            tokio::select! {changed=stop.changed()=>if changed.is_err(){return false},event=wake.recv()=>match event{Some(Wake::Maintenance)=>self.clear_maintenance(id),Some(_)=>{},None=>return false}}
        }
    }
    async fn observe(&self, input: Observation) -> Result<(), Failure> {
        let journal = self.resources.journal.clone();
        tokio::task::spawn_blocking(move || {
            journal
                .lock()
                .map_err(|_| "runtime_journal_unavailable")?
                .observe_native(input, now())
        })
        .await
        .map_err(|_| "runtime_journal_unavailable")?
    }
}
fn retry_attempt() -> Result<String, Failure> {
    let mut attempt = token()?;
    attempt.truncate(48);
    Ok(attempt)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn random_attempt_matches_the_existing_journal_protocol() {
        let attempt = retry_attempt().unwrap();
        assert_eq!(attempt.len(), 48);
        assert!(
            attempt
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        let mut journal = Journal::default();
        let scheduled = journal
            .schedule_native_retry(
                "profile_fixture".into(),
                "native-owner".into(),
                attempt.clone(),
                2000,
                1000,
            )
            .unwrap();
        assert_eq!(scheduled.generation, 1);
        assert!(scheduled.due_at_ms.is_some());
        assert_eq!(
            journal.next_due_retry(i64::MAX).unwrap().attempt_id,
            attempt
        );
    }
}
