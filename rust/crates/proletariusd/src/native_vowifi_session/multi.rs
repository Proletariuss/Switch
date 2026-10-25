//! Retained SMS-only profile owners. Waiter cancellation requests stop, but the
//! runtime keeps preparation, consumers and card cleanup until their real joins.
use super::*;
use crate::ims_sms_transactions::NativeSmsArchive;
pub(crate) use crate::multistandby_native_card::CleanupReceipt;
use crate::multistandby_native_card::{Pool, Profile};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, watch};
mod startup;
pub(crate) use super::ordinary::status::{EndReason, RetryClass};
use startup::Starts;
pub(crate) use startup::{StartFailure, Startup};

pub(crate) struct Runtime {
    pool: Arc<Pool>,
    gate: Arc<crate::reader_reservation::Gate>,
    root: PathBuf,
    transactions: crate::ims_sms_transactions::Service,
    calls: crate::call_facts::CallFacts,
    owner: Arc<Mutex<Option<NativeSessionServiceOwner>>>,
    starts: Mutex<Starts>,
    network_joined: Arc<std::sync::atomic::AtomicBool>,
    finished: watch::Receiver<Option<Result<CleanupReceipt, String>>>,
    completion: Mutex<Option<watch::Sender<Option<Result<CleanupReceipt, String>>>>>,
}
impl Runtime {
    pub(crate) fn new(
        pool: Arc<Pool>,
        gate: Arc<crate::reader_reservation::Gate>,
        root: PathBuf,
        transactions: crate::ims_sms_transactions::Service,
    ) -> Result<Arc<Self>, &'static str> {
        let owner = NativeSessionServiceOwner::new(Duration::from_secs(1))?;
        let (send, finished) = watch::channel(None);
        Ok(Arc::new(Self {
            pool,
            gate,
            calls: crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(root.join("native-multi-calls.json")),
                "native-multi".into(),
            ),
            root,
            transactions,
            owner: Arc::new(Mutex::new(Some(owner))),
            starts: Mutex::new(Starts::default()),
            network_joined: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            finished,
            completion: Mutex::new(Some(send)),
        }))
    }
    #[cfg(test)]
    pub(crate) fn cleanup_watch_for_test(
        &self,
    ) -> watch::Receiver<Option<Result<CleanupReceipt, String>>> {
        self.finished.clone()
    }
    pub(crate) fn begin_start(
        self: &Arc<Self>,
        profile: Profile,
        request: PlanRequest,
        stop: watch::Receiver<bool>,
    ) -> Result<Startup, StartFailure> {
        self.begin_start_scoped(profile, request, stop, NativeSmsArchive::Ordinary)
    }
    pub(crate) fn begin_start_for_profile(
        self: &Arc<Self>,
        profile: Profile,
        request: PlanRequest,
        stop: watch::Receiver<bool>,
        store: crate::multistandby_store::Store,
        profile_id: String,
    ) -> Result<Startup, StartFailure> {
        self.begin_start_scoped(
            profile,
            request,
            stop,
            NativeSmsArchive::Nwait { store, profile_id },
        )
    }
    pub(crate) async fn start(
        self: &Arc<Self>,
        profile: Profile,
        request: PlanRequest,
        stop: watch::Receiver<bool>,
    ) -> Result<NativeMultiSession, String> {
        self.begin_start(profile, request, stop)
            .map_err(|e| e.code)?
            .wait()
            .await
            .map_err(|e| e.code)
    }
    pub(crate) async fn start_for_profile(
        self: &Arc<Self>,
        profile: Profile,
        request: PlanRequest,
        stop: watch::Receiver<bool>,
        store: crate::multistandby_store::Store,
        profile_id: String,
    ) -> Result<NativeMultiSession, String> {
        self.begin_start_for_profile(profile, request, stop, store, profile_id)
            .map_err(|e| e.code)?
            .wait()
            .await
            .map_err(|e| e.code)
    }
    async fn prepare(
        &self,
        profile: Profile,
        request: PlanRequest,
        stop: watch::Receiver<bool>,
        archive: NativeSmsArchive,
    ) -> Result<ordinary::Handle, StartFailure> {
        let card = self
            .pool
            .prepare_retained(profile, Some(stop.clone()))
            .await
            .map_err(|e| StartFailure {
                code: e.error.to_string(),
                cleanup_confirmed: e.cleanup_confirmed,
                retry: RetryClass::RetryAfterRelease,
            })?;
        let card = SessionCard::on_demand(card);
        let context =
            match prepare_card_context_with_stop(request, card, true, Some(stop.clone())).await {
                Ok(context) => context,
                Err((error, card)) => {
                    return Err(StartFailure::release(
                        error.code(),
                        card,
                        if error.retryable() {
                            RetryClass::RetryAfterRelease
                        } else {
                            RetryClass::Permanent
                        },
                    )
                    .await);
                }
            };
        let settings = match service::startup_settings() {
            Ok(settings) => settings,
            Err(code) => {
                return Err(StartFailure::release(code, context.card, RetryClass::Permanent).await);
            }
        };
        if *stop.borrow() || stop.has_changed().is_err() {
            return Err(StartFailure::release(
                "native multi startup stopped",
                context.card,
                RetryClass::Stop,
            )
            .await);
        }
        let inputs = ordinary::Inputs {
            sms_archive: archive,
            reader_gate: self.gate.clone(),
            settings,
            data_root: self.root.clone(),
            transactions: self.transactions.clone(),
            call_facts: self.calls.clone(),
        };
        let result = {
            let mut owner = self.owner.lock().unwrap();
            match owner.as_mut() {
                Some(owner) => owner.launch_profile(context, inputs, true),
                None => Err((NativeSessionAdmissionError::Stopping, context)),
            }
        };
        match result {
            Ok(handle) => Ok(handle),
            Err((_, context)) => Err(StartFailure::release(
                "native multi session admission rejected",
                context.card,
                RetryClass::Stop,
            )
            .await),
        }
    }
    pub(crate) fn begin_shutdown(&self) {
        let mut starts = self.starts.lock().unwrap();
        if starts.stopping {
            return;
        }
        starts.stopping = true;
        for task in &starts.tasks {
            task.stop.send_replace(true);
        }
        let tasks = std::mem::take(&mut starts.tasks);
        let prior_failure = starts.failure.take();
        if let Some(owner) = self.owner.lock().unwrap().as_mut() {
            let _ = owner.begin_shutdown();
        }
        let send = self
            .completion
            .lock()
            .unwrap()
            .take()
            .expect("one native shutdown owner");
        let owner_state = self.owner.clone();
        let pool = self.pool.clone();
        let network_joined = self.network_joined.clone();
        tokio::spawn(async move {
            let mut failure = prior_failure;
            // Includes unpublished preparations and each profile's real consumer join.
            for task in tasks {
                match task.join.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        failure.get_or_insert(error);
                    }
                    Err(error) => {
                        failure.get_or_insert(error.to_string());
                    }
                }
            }
            let mut owner = owner_state
                .lock()
                .unwrap()
                .take()
                .expect("retained native owner");
            loop {
                match owner
                    .finish_before(tokio::time::Instant::now() + Duration::from_secs(30))
                    .await
                {
                    Ok(Ok(_)) => break,
                    Ok(Err(error)) => {
                        failure.get_or_insert(error.to_string());
                        break;
                    }
                    Err((_, retained)) => owner = retained,
                }
            }
            if let Some(error) = failure {
                send.send_replace(Some(Err(error)));
                return;
            }
            network_joined.store(true, std::sync::atomic::Ordering::Release);
            send.send_replace(Some(
                pool.shutdown_receipt().await.map_err(|e| e.to_string()),
            ));
        });
    }
    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        self.shutdown_receipt().await.map(|_| ())
    }
    pub(crate) async fn retry_shutdown_receipt(&self) -> Result<CleanupReceipt, String> {
        match self.shutdown_receipt().await {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                if !self
                    .network_joined
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err(error);
                }
                self.pool
                    .retry_shutdown_receipt()
                    .await
                    .map_err(|e| e.to_string())
            }
        }
    }
    pub(crate) async fn shutdown_receipt(&self) -> Result<CleanupReceipt, String> {
        self.begin_shutdown();
        let mut result = self.finished.clone();
        loop {
            if let Some(value) = result.borrow().clone() {
                return value;
            }
            result
                .changed()
                .await
                .map_err(|_| "native multi shutdown unconfirmed")?;
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.begin_shutdown();
    }
}

#[derive(Clone)]
pub(crate) struct SessionExit {
    pub(crate) snapshot: ordinary::status::Snapshot,
    pub(crate) joined: bool,
    pub(crate) cleanup_confirmed: bool,
}
pub(crate) struct NativeMultiSession {
    handle: ordinary::Handle,
    exit: watch::Receiver<Option<SessionExit>>,
}
impl NativeMultiSession {
    pub(crate) fn snapshot(&self) -> ordinary::status::Snapshot {
        self.handle.snapshot()
    }
    pub(crate) fn status_watch(&self) -> ordinary::Handle {
        self.handle.clone()
    }
    pub(crate) fn controls(
        &self,
    ) -> tokio::sync::mpsc::Sender<crate::native_registered_session::Control> {
        self.handle.controls.clone()
    }
    pub(crate) fn generation(&self) -> crate::native_vowifi_supervisor::SessionGeneration {
        self.handle.generation
    }
    pub(crate) fn request_stop(&self) {
        self.handle.request_stop();
    }
    pub(crate) async fn wait_exit(&self) -> SessionExit {
        let mut exit = self.exit.clone();
        loop {
            if let Some(value) = exit.borrow().clone() {
                return value;
            }
            if exit.changed().await.is_err() {
                return SessionExit {
                    snapshot: self.snapshot(),
                    joined: false,
                    cleanup_confirmed: false,
                };
            }
        }
    }
    pub(crate) async fn stop_and_join(&self) -> SessionExit {
        self.request_stop();
        self.wait_exit().await
    }
    pub(crate) async fn finish(&self) -> Result<(), String> {
        let exit = self.stop_and_join().await;
        if exit.joined && exit.cleanup_confirmed {
            Ok(())
        } else {
            Err("native multi session cleanup unconfirmed".into())
        }
    }
}
impl Drop for NativeMultiSession {
    fn drop(&mut self) {
        self.handle.request_shutdown();
    }
}
