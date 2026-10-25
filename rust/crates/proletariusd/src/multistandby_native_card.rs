//! A native business owner and FIFO of short physical workflows. A profile
//! retains immutable identity, never a PC/SC pin between network challenges.
use crate::{
    multistandby_card::{NativeAction, NativeRequest, NativeResult, Service},
    native_vowifi_session::card::{CardFuture, OnDemandCard},
    reader_pcsc::Owner,
    reader_reservation::{Gate, NativeMultiBinding},
    reader_session_lease::{LeaseIdentity, ReaderAkaApplication},
};
use proletarius_domain::BackendFailure;
use proletarius_vowifi::SimAkaResult;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};

#[derive(Clone)]
pub(crate) struct Profile {
    pub(crate) eid: String,
    pub(crate) aid: String,
    pub(crate) iccid: String,
}
/// Terminal cleanup facts; confirmed loss never claims the original profile
/// was restored. These values are published only after the retained FIFO and
/// the Gate's exact physical-operation retirement checks have completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CleanupReceipt {
    /// The original profile was reselected and its live identity verified.
    OriginalRestored,
    /// No profile-changing work began, so no original-profile restoration ran.
    NoProfileWork,
    /// The captured reader session closed; restoration was deliberately skipped.
    ConfirmedReaderLost,
}
pub(crate) struct PreparationFailure {
    pub(crate) error: BackendFailure,
    pub(crate) cleanup_confirmed: bool,
}
fn failure(message: impl ToString) -> BackendFailure {
    BackendFailure::Io(message.to_string())
}
struct Requests {
    released: bool,
    cleanup_unconfirmed: bool,
    active: HashMap<u64, Arc<AtomicBool>>,
}
struct Session {
    requests: Mutex<Requests>,
    changed: Notify,
    lost: watch::Sender<bool>,
}
impl Session {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Requests {
                released: false,
                cleanup_unconfirmed: false,
                active: HashMap::new(),
            }),
            changed: Notify::new(),
            lost: watch::channel(false).0,
        })
    }
    fn cancel(&self) {
        let mut requests = self.requests.lock().unwrap();
        requests.released = true;
        self.lost.send_replace(true);
        for flag in requests.active.values() {
            flag.store(true, Ordering::Release);
        }
    }
    async fn release(&self) -> Result<(), BackendFailure> {
        self.cancel();
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let requests = self.requests.lock().unwrap();
                if requests.active.is_empty() {
                    return if requests.cleanup_unconfirmed {
                        Err(failure("native physical cleanup is unconfirmed"))
                    } else {
                        Ok(())
                    };
                }
            }
            changed.await;
        }
    }
}
struct Completed {
    session: Arc<Session>,
    id: u64,
    confirmed: bool,
}
impl Drop for Completed {
    fn drop(&mut self) {
        let mut requests = self.session.requests.lock().unwrap();
        requests.active.remove(&self.id);
        if !self.confirmed {
            requests.cleanup_unconfirmed = true;
            requests.released = true;
            self.session.lost.send_replace(true);
        }
        drop(requests);
        self.session.changed.notify_waiters();
    }
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
struct Job {
    profile: Profile,
    imsi: String,
    action: NativeAction,
    cancelled: Arc<AtomicBool>,
    completed: Completed,
    reply: oneshot::Sender<Result<NativeResult, String>>,
    _slot: OwnedSemaphorePermit,
}
enum Command {
    Work(Job),
    Shutdown,
}
struct Admission {
    stopping: bool,
    sessions: Vec<Weak<Session>>,
}
pub(crate) struct Pool {
    gate: Arc<Gate>,
    parent: NativeMultiBinding,
    reader: String,
    generation: u64,
    tx: mpsc::Sender<Command>,
    admission: Mutex<Admission>,
    sequence: AtomicU64,
    slots: Arc<Semaphore>,
    restore_pending: Arc<AtomicBool>,
    finished: watch::Receiver<Option<Result<CleanupReceipt, String>>>,
}
struct Worker {
    owner: Owner,
    gate: Arc<Gate>,
    service: Service,
    parent: NativeMultiBinding,
    original: Option<(Profile, String)>,
    restore_pending: Arc<AtomicBool>,
}
impl Pool {
    pub(crate) fn acquire(
        owner: Owner,
        gate: Arc<Gate>,
        service: Service,
        reader: String,
        generation: u64,
    ) -> Result<Arc<Self>, BackendFailure> {
        let parent = gate
            .acquire_native_multi(&reader, generation)
            .map_err(failure)?;
        let (tx, rx) = mpsc::channel(65);
        let restore_pending = Arc::new(AtomicBool::new(false));
        let (finished, receive) = watch::channel(None);
        let pool = Arc::new(Self {
            gate: gate.clone(),
            parent: parent.clone(),
            reader,
            generation,
            tx,
            admission: Mutex::new(Admission {
                stopping: false,
                sessions: vec![],
            }),
            sequence: AtomicU64::new(1),
            slots: Arc::new(Semaphore::new(64)),
            restore_pending: restore_pending.clone(),
            finished: receive,
        });
        tokio::spawn(
            Worker {
                owner,
                gate,
                service,
                parent,
                original: None,
                restore_pending,
            }
            .run(rx, finished),
        );
        let weak = Arc::downgrade(&pool);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Some(pool) = weak.upgrade() else {
                    return;
                };
                if pool.finished.borrow().is_some() {
                    return;
                }
                if !pool.gate.native_multi_current(&pool.parent) {
                    pool.begin_shutdown();
                    return;
                }
            }
        });
        Ok(pool)
    }
    pub(crate) async fn prepare(
        self: &Arc<Self>,
        profile: Profile,
    ) -> Result<Arc<ProfileCard>, BackendFailure> {
        self.prepare_with_stop(profile, None).await
    }
    pub(crate) async fn prepare_with_stop(
        self: &Arc<Self>,
        profile: Profile,
        stop: Option<watch::Receiver<bool>>,
    ) -> Result<Arc<ProfileCard>, BackendFailure> {
        self.prepare_retained(profile, stop)
            .await
            .map_err(|failure| failure.error)
    }
    /// Every failed preparation returns only after its physical job drains.
    pub(crate) async fn prepare_retained(
        self: &Arc<Self>,
        profile: Profile,
        stop: Option<watch::Receiver<bool>>,
    ) -> Result<Arc<ProfileCard>, PreparationFailure> {
        let session = Session::new();
        {
            let mut admission = self.admission.lock().unwrap();
            if admission.stopping {
                return Err(PreparationFailure {
                    error: failure("native multi pool is stopping"),
                    cleanup_confirmed: true,
                });
            }
            admission.sessions.retain(|s| s.strong_count() > 0);
            if admission.sessions.len() >= 32 {
                return Err(PreparationFailure {
                    error: failure("native profile capacity exhausted"),
                    cleanup_confirmed: true,
                });
            }
            admission.sessions.push(Arc::downgrade(&session));
        }
        let request = self.submit(
            session.clone(),
            profile.clone(),
            String::new(),
            NativeAction::Prepare,
        );
        tokio::pin!(request);
        let result = if let Some(mut stop) = stop {
            let stopped = async {
                loop {
                    if *stop.borrow() || stop.changed().await.is_err() {
                        break;
                    }
                }
            };
            tokio::select! {
                biased;
                _ = stopped => Err(failure("native profile preparation stopped")),
                result = &mut request => result,
            }
        } else {
            request.await
        };
        let result = match result {
            Ok(result) if !session.requests.lock().unwrap().released => result,
            value => {
                let error = value
                    .err()
                    .unwrap_or_else(|| failure("profile preparation cancelled"));
                let cleanup_confirmed = session.release().await.is_ok();
                return Err(PreparationFailure {
                    error,
                    cleanup_confirmed,
                });
            }
        };
        Ok(Arc::new(ProfileCard {
            pool: self.clone(),
            profile,
            identity: result.identity,
            session,
        }))
    }
    fn submit(
        &self,
        session: Arc<Session>,
        profile: Profile,
        imsi: String,
        action: NativeAction,
    ) -> CardFuture<NativeResult> {
        let (reply, wait) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let admission = self.admission.lock().unwrap();
        let mut requests = session.requests.lock().unwrap();
        if admission.stopping || requests.released || !self.gate.native_multi_current(&self.parent)
        {
            return Box::pin(async { Err(failure("native profile capability has retired")) });
        }
        if requests.active.len() >= 8 {
            return Box::pin(async { Err(failure("native profile request capacity exhausted")) });
        }
        let Ok(slot) = self.slots.clone().try_acquire_owned() else {
            return Box::pin(async { Err(failure("native card FIFO capacity exhausted")) });
        };
        let Ok(id) = self
            .sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
        else {
            return Box::pin(async { Err(failure("native request sequence exhausted")) });
        };
        requests.active.insert(id, cancelled.clone());
        drop(requests);
        let job = Job {
            profile,
            imsi,
            action,
            cancelled: cancelled.clone(),
            completed: Completed {
                session,
                id,
                confirmed: false,
            },
            reply,
            _slot: slot,
        };
        let sent = self.tx.try_send(Command::Work(job)).map_err(|error| {
            if let Command::Work(mut job) = error.into_inner() {
                job.completed.confirmed = true;
            }
        });
        drop(admission);
        let cancel = CancelOnDrop(cancelled);
        Box::pin(async move {
            let _cancel = cancel;
            if sent.is_err() {
                return Err(failure("native card FIFO has stopped"));
            }
            wait.await
                .map_err(|_| failure("native card worker did not return"))?
                .map_err(failure)
        })
    }
    pub(crate) fn begin_shutdown(&self) {
        let mut admission = self.admission.lock().unwrap();
        if admission.stopping {
            return;
        }
        admission.stopping = true;
        for session in admission.sessions.iter().filter_map(Weak::upgrade) {
            session.cancel();
        }
        self.restore_pending.store(true, Ordering::Release);
        let _ = self.tx.try_send(Command::Shutdown);
    }
    /// Compatibility result means safe resource retirement, not profile restoration.
    pub(crate) async fn retry_shutdown(&self) -> Result<(), BackendFailure> {
        self.retry_shutdown_receipt().await.map(|_| ())
    }
    pub(crate) async fn retry_shutdown_receipt(&self) -> Result<CleanupReceipt, BackendFailure> {
        let mut finished = self.finished.clone();
        finished.borrow_and_update();
        if !matches!(&*finished.borrow(), Some(Err(_))) {
            return self.shutdown_receipt().await;
        }
        if self
            .restore_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.tx
                .try_send(Command::Shutdown)
                .map_err(|_| failure("native pool has stopped"))?;
        }
        finished
            .changed()
            .await
            .map_err(|_| failure("native pool cleanup was not confirmed"))?;
        let result = finished
            .borrow()
            .clone()
            .ok_or_else(|| failure("native cleanup missing result"))?;
        result.map_err(failure)
    }
    /// Compatibility result means safe resource retirement, not profile restoration.
    /// Consumers needing restoration facts must use `shutdown_receipt`.
    pub(crate) async fn shutdown(&self) -> Result<(), BackendFailure> {
        self.shutdown_receipt().await.map(|_| ())
    }
    pub(crate) async fn shutdown_receipt(&self) -> Result<CleanupReceipt, BackendFailure> {
        self.begin_shutdown();
        let mut finished = self.finished.clone();
        loop {
            if let Some(result) = finished.borrow().clone() {
                return result.map_err(failure);
            }
            finished
                .changed()
                .await
                .map_err(|_| failure("native pool cleanup was not confirmed"))?;
        }
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        self.begin_shutdown();
    }
}
pub(crate) struct ProfileCard {
    pool: Arc<Pool>,
    profile: Profile,
    identity: LeaseIdentity,
    session: Arc<Session>,
}
impl Drop for ProfileCard {
    fn drop(&mut self) {
        self.session.cancel();
    }
}
impl OnDemandCard for ProfileCard {
    fn reader(&self) -> &str {
        &self.pool.reader
    }
    fn reader_generation(&self) -> u64 {
        self.pool.generation
    }
    fn identity(&self) -> &LeaseIdentity {
        &self.identity
    }
    fn lost_receiver(&self) -> watch::Receiver<bool> {
        self.session.lost.subscribe()
    }
    fn authenticate(
        self: Arc<Self>,
        application: ReaderAkaApplication,
        rand: [u8; 16],
        autn: [u8; 16],
    ) -> CardFuture<SimAkaResult> {
        let request = self.pool.submit(
            self.session.clone(),
            self.profile.clone(),
            self.identity.imsi.to_string(),
            NativeAction::Authenticate {
                application,
                rand,
                autn,
            },
        );
        Box::pin(async move {
            request
                .await?
                .aka
                .ok_or_else(|| failure("missing native AKA result"))
        })
    }
    fn release_with_confirmation(self: Arc<Self>) -> CardFuture<()> {
        // Mark retired synchronously, so even a dropped release waiter closes
        // admission while the FIFO retains physical cleanup independently.
        self.session.cancel();
        Box::pin(async move { self.session.release().await })
    }
}
impl Worker {
    async fn physical(
        &self,
        profile: &Profile,
        imsi: String,
        action: NativeAction,
        cancelled: Arc<AtomicBool>,
        confirmed: &mut bool,
    ) -> Result<NativeResult, String> {
        if cancelled.load(Ordering::Acquire) {
            return Err("native card request cancelled".into());
        }
        let operation = self
            .gate
            .begin_native_multi_operation(self.owner.clone(), &self.parent)
            .map_err(str::to_owned)?;
        *confirmed = false;
        let body = async {
            self.gate
                .acquire_native_multi_pcsc(self.owner.clone(), &operation, cancelled.clone())
                .await
                .map_err(|e| e.to_string())?;
            self.service
                .run_native(
                    NativeRequest {
                        transaction: operation.transaction.clone(),
                        pcsc: operation.pcsc.clone(),
                        eid: profile.eid.clone(),
                        aid: profile.aid.clone(),
                        iccid: profile.iccid.clone(),
                        expected_imsi: imsi,
                        action,
                    },
                    cancelled.clone(),
                )
                .await
                .map_err(|e| e.to_string())
        };
        tokio::pin!(body);
        let result = loop {
            tokio::select! {
                result = &mut body => break result,
                _ = tokio::time::sleep(Duration::from_secs(20)) => {
                    if !cancelled.load(Ordering::Acquire) {
                        let logical = self.gate.renew_native_multi_operation(&operation);
                        let physical = operation.pcsc.as_ref().map(|b| self.owner.renew(b, Duration::from_secs(120)));
                        if logical.is_err() || physical.is_some_and(|r| r.is_err()) { cancelled.store(true, Ordering::Release); }
                    }
                }
            }
        };
        // This joins the physical release receipt as well as the workflow job.
        // FIFO ownership is retained on every error and on waiter cancellation.
        self.gate
            .finish_native_multi_operation(&operation)
            .await
            .map_err(str::to_owned)?;
        *confirmed = true;
        if cancelled.load(Ordering::Acquire) {
            return Err("native card request cancelled".into());
        }
        result
    }
    async fn run(
        mut self,
        mut rx: mpsc::Receiver<Command>,
        finished: watch::Sender<Option<Result<CleanupReceipt, String>>>,
    ) {
        while let Some(command) = rx.recv().await {
            match command {
                Command::Shutdown => {
                    let restore = self.restore().await;
                    self.restore_pending.store(false, Ordering::Release);
                    finished.send_replace(Some(restore.clone()));
                    if restore.is_ok() {
                        return;
                    }
                    continue;
                }
                Command::Work(job) => {
                    let Job {
                        profile,
                        imsi,
                        action,
                        cancelled,
                        mut completed,
                        reply,
                        _slot,
                    } = job;
                    completed.confirmed = true; // no submitted physical work yet
                    let result = async {
                        if let Some((original, _)) = &self.original {
                            if original.eid != profile.eid {
                                return Err("native pool eUICC identity changed".into());
                            }
                        } else {
                            let current = self
                                .physical(
                                    &profile,
                                    String::new(),
                                    NativeAction::Identity,
                                    cancelled.clone(),
                                    &mut completed.confirmed,
                                )
                                .await?;
                            self.original = Some((
                                Profile {
                                    iccid: current.identity.iccid.to_string(),
                                    ..profile.clone()
                                },
                                current.identity.imsi.to_string(),
                            ));
                        }
                        self.physical(&profile, imsi, action, cancelled, &mut completed.confirmed)
                            .await
                    }
                    .await;
                    drop(completed);
                    let _ = reply.send(result);
                }
            }
        }
        // Last owner disappeared: still attempt actual restoration and cleanup.
        let result = self.restore().await;
        finished.send_replace(Some(result));
    }
    fn retire_confirmed_loss(&self) -> Result<CleanupReceipt, String> {
        match self
            .gate
            .release_native_multi_after_confirmed_loss(&self.parent)
        {
            Ok(true) => Ok(CleanupReceipt::ConfirmedReaderLost),
            Ok(false) => Err("native reader changed without confirmed physical loss".into()),
            Err(error) => Err(error.into()),
        }
    }
    async fn restore(&self) -> Result<CleanupReceipt, String> {
        if !self.gate.native_multi_current(&self.parent) {
            return self.retire_confirmed_loss();
        }
        let restored = async {
            let receipt = if let Some((profile, imsi)) = &self.original {
                let mut confirmed = true;
                let result = self
                    .physical(
                        profile,
                        String::new(),
                        NativeAction::Prepare,
                        Arc::new(AtomicBool::new(false)),
                        &mut confirmed,
                    )
                    .await?;
                if result.identity.imsi.as_str() != imsi {
                    return Err("original profile identity changed during restoration".into());
                }
                CleanupReceipt::OriginalRestored
            } else {
                CleanupReceipt::NoProfileWork
            };
            self.gate
                .release_native_multi(&self.parent)
                .map_err(str::to_owned)?;
            Ok(receipt)
        }
        .await;
        // Disappearance can commit after the initial check or after a failed
        // restoration workflow actually joins. Generation change alone is not
        // enough: consume only the exact parent-pinned successful close proof.
        if restored.is_err() && !self.gate.native_multi_current(&self.parent) {
            return self.retire_confirmed_loss();
        }
        restored
    }
}

#[cfg(test)]
mod tests;
