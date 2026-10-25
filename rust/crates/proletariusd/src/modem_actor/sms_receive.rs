//! Rust owns stored-SMS event scheduling. The serial actor only queues work;
//! the one async worker reuses its fenced commands and the durable ingest core.
use super::*;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Notify;

const MAX_RECEIVE_JOBS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReceiveFence {
    pub(super) session: u64,
    pub(super) receive_epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReceiveWork {
    id: u64,
    fence: ReceiveFence,
    generation: u64,
    storage: Option<String>,
    index: Option<u32>,
}

pub(super) struct ReceiveQueue {
    enabled: bool,
    pub(super) configured: bool,
    epoch: u64,
    next_id: u64,
    pending: VecDeque<ReceiveWork>,
    active: Option<ReceiveWork>,
    wake: Arc<Notify>,
}

impl ReceiveQueue {
    pub(super) fn native_dirty_storages(&self) -> Vec<String> {
        self.pending
            .iter()
            .filter_map(|job| job.storage.clone())
            .collect()
    }

    pub(super) fn native_rescan(
        &mut self,
        session: u64,
        generation: u64,
        storages: Vec<String>,
    ) -> Result<(), ActorError> {
        // Preserve device sources, replace stale slot claims with fresh scans.
        self.pending.clear();
        self.push(session, generation, None, None)?;
        for storage in storages {
            self.push(session, generation, Some(storage), None)?;
        }
        Ok(())
    }

    pub(super) fn new(wake: Arc<Notify>) -> Self {
        Self {
            enabled: false,
            configured: false,
            epoch: 1,
            next_id: 0,
            pending: VecDeque::new(),
            active: None,
            wake,
        }
    }

    pub(super) fn invalidate(&mut self) {
        self.enabled = false;
        // Zero is a permanently disabled epoch for this actor on exhaustion.
        if self.epoch != 0 {
            self.epoch = self.epoch.checked_add(1).unwrap_or(0);
        }
        self.pending.clear();
        self.wake.notify_one();
    }

    pub(super) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn wake_worker(&self) {
        self.wake.notify_one();
    }

    fn push(
        &mut self,
        session: u64,
        generation: u64,
        storage: Option<String>,
        index: Option<u32>,
    ) -> Result<(), ActorError> {
        let fence = ReceiveFence {
            session,
            receive_epoch: self.epoch,
        };
        if self.epoch == 0 {
            return Err(ActorError::unavailable("SMS receive epoch exhausted"));
        }
        // Never coalesce against an active snapshot: another notification may
        // refer to a newly reused slot which that snapshot did not contain.
        if self.pending.iter().any(|job| {
            job.fence == fence
                && job.generation == generation
                && job.storage == storage
                && (job.index.is_none() || job.index == index)
        }) {
            return Ok(());
        }
        if self.pending.len() >= MAX_RECEIVE_JOBS {
            // A full mailbox becomes one complete scan of the same storage.
            // Different storage identities never overwrite one another.
            if let Some(job) = self.pending.iter_mut().find(|job| {
                job.fence == fence && job.generation == generation && job.storage == storage
            }) {
                job.index = None;
                self.wake.notify_one();
                return Ok(());
            }
            return Err(ActorError::new(
                "sms_receive_queue_full",
                "SMS receive storage mailbox is full; sources retained",
            ));
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| ActorError::unavailable("SMS receive job sequence exhausted"))?;
        self.pending.push_back(ReceiveWork {
            id: self.next_id,
            fence,
            generation,
            storage,
            index,
        });
        self.wake.notify_one();
        Ok(())
    }

    pub(super) fn observe(
        &mut self,
        session: u64,
        generation: u64,
        event: Option<&SmsStorageEvent>,
    ) -> Result<bool, ActorError> {
        let Some(event) = event.filter(|_| self.enabled) else {
            return Ok(false);
        };
        let Some(storage) = normalize_sms_storage(&event.storage) else {
            return Ok(false);
        };
        let Ok(index) = event.index.parse::<u32>() else {
            return Ok(false);
        };
        if index == 0 {
            return Ok(false);
        }
        if let Err(error) = self.push(session, generation, Some(storage), Some(index)) {
            // Overload is a receive failure, not a transport disconnect or
            // permission for Go to start a second unbounded receive pipeline.
            crate::diagnostics::write_line(format_args!(
                "switchd-rs: stored SMS notification retained on device ({})",
                error.code
            ));
        }
        Ok(true)
    }

    pub(super) fn generation_changed(&mut self, generation: u64) {
        self.pending.retain(|job| job.generation == generation);
    }
}

pub(super) struct ActorClient {
    pub(super) module_recovery: crate::module_recovery::Gate,
    pub(super) final_close: Mutex<Option<Result<(), ActorError>>>,
    pub(super) actor_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    pub(super) native_shared: Arc<NativeShared>,
    pub(super) card_shared: Arc<CardShared>,
    pub(super) module_esim_cache: Arc<crate::module_esim_transport::SerialCache>,
    pub(super) live_serial_session: Arc<std::sync::atomic::AtomicU64>,
    pub(super) call_capture: Arc<std::sync::Mutex<Option<crate::module_radio::CallCapture>>>,
    pub(super) cellular_suppressed: Arc<std::sync::atomic::AtomicBool>,
    pub(super) sender: SyncSender<Command>,
    pub(super) wake: Arc<Notify>,
}
impl Drop for ActorClient {
    fn drop(&mut self) {
        self.wake.notify_one();
    }
}

#[derive(Default)]
pub(super) struct ReceiveWorker {
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    stopping: AtomicBool,
    finish: tokio::sync::Mutex<ReceiveFinish>,
}

#[derive(Default)]
enum ReceiveFinish {
    #[default]
    NotStarted,
    Joining(tokio::task::JoinHandle<()>),
    Complete(Result<(), ActorError>),
}

impl ReceiveWorker {
    async fn finish(&self) -> Result<(), ActorError> {
        let mut finish = self.finish.lock().await;
        if matches!(*finish, ReceiveFinish::NotStarted) {
            *finish = match self.task.lock().expect("SMS worker mutex").take() {
                Some(task) => ReceiveFinish::Joining(task),
                None => ReceiveFinish::Complete(Ok(())),
            };
        }
        let result = match &mut *finish {
            // Borrow the retained handle across await. Cancellation releases
            // only this waiter; another caller still joins the same worker.
            ReceiveFinish::Joining(task) => task.await.map_err(|error| {
                ActorError::new(
                    "serial_sms_receiver_join_failed",
                    format!("stored SMS receive worker failed: {error}"),
                )
            }),
            ReceiveFinish::Complete(result) => return result.clone(),
            ReceiveFinish::NotStarted => unreachable!("receive finish initialized above"),
        };
        *finish = ReceiveFinish::Complete(result.clone());
        result
    }
}

impl ModemActor {
    pub(super) fn ensure_receive_worker(&self, store: proletarius_store::ArchiveStore) {
        let mut slot = self.receive_worker.task.lock().expect("SMS worker mutex");
        if self.receive_worker.stopping.load(Ordering::Acquire) {
            return;
        }
        if slot.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let client = Arc::downgrade(&self.client);
        let worker = self.receive_worker.clone();
        let wake = self.client.wake.clone();
        *slot = Some(tokio::spawn(receive_loop(client, worker, wake, store)));
    }

    pub(super) fn configure_sms_receive(&self, enabled: bool) -> Result<(), ActorError> {
        self.submit(
            |reply| Command::ConfigureSmsReceive { enabled, reply },
            RPC_TIMEOUT,
        )
    }
    pub(super) fn scan_sms_receive(&self) -> Result<(), ActorError> {
        self.submit(|reply| Command::ScanSmsReceive { reply }, RPC_TIMEOUT)
    }

    pub(crate) async fn finish_sms_receiver(&self) -> Result<(), ActorError> {
        self.receive_worker.stopping.store(true, Ordering::Release);
        self.client.wake.notify_one();
        // After serial close, queued work has been revoked. An archive
        // already being written must finish; abort is not an I/O cancel.
        self.receive_worker.finish().await
    }
}

async fn receive_loop(
    client: Weak<ActorClient>,
    worker: Arc<ReceiveWorker>,
    wake: Arc<Notify>,
    store: proletarius_store::ArchiveStore,
) {
    loop {
        if worker.stopping.load(Ordering::Acquire) {
            break;
        }
        // Subscribe before checking the queue; notify_one retains one permit
        // if the frame arrives before this future starts waiting.
        let notified = wake.notified();
        let Some(strong) = client.upgrade() else {
            break;
        };
        let actor = ModemActor {
            compatibility_rpc: false,
            client: strong,
            receive_worker: worker.clone(),
        };
        let next_actor = actor.clone();
        let next = tokio::task::spawn_blocking(move || {
            next_actor.submit(|reply| Command::NextSmsReceive { reply }, RPC_TIMEOUT)
        })
        .await;
        let job = match next {
            Ok(Ok(Some(job))) => job,
            Ok(Ok(None)) => {
                drop(actor);
                notified.await;
                continue;
            }
            Ok(Err(error)) if error.code == "serial_actor_stopped" => break,
            _ => {
                drop(actor);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let result = super::sms_ingest::ingest(
            IngestStoredSmsRequest {
                storage: job.storage.clone(),
                index: job.index,
                expected_sms_generation: job.generation,
            },
            actor.clone(),
            store.clone(),
            Some(job.fence),
        )
        .await;
        if let Err(response) = result {
            // Do not emit identifiers, PDU or message contents. Failure keeps
            // the card source; later notifications/manual scan can retry.
            crate::diagnostics::write_line(format_args!(
                "switchd-rs: native stored SMS receive not confirmed (HTTP {})",
                response.status().as_u16()
            ));
        }
        // Completion is idempotent by job id. Do not start a second ingest
        // when the mailbox acknowledgement is uncertain.
        loop {
            let finish_actor = actor.clone();
            let id = job.id;
            match tokio::task::spawn_blocking(move || {
                finish_actor.submit(|reply| Command::FinishSmsReceive { id, reply }, RPC_TIMEOUT)
            })
            .await
            {
                Ok(Ok(())) => break,
                Ok(Err(error)) if error.code == "serial_actor_stopped" => return,
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }
}

impl ActorState {
    pub(super) fn wake_sms_receive(&self) {
        self.sms_receive.wake_worker();
    }

    pub(super) fn check_receive_fence(
        &self,
        fence: Option<ReceiveFence>,
    ) -> Result<(), ActorError> {
        if let Some(fence) = fence {
            if self.port.is_none()
                || fence.session != self.serial_session
                || fence.receive_epoch != self.sms_receive.epoch
                || self.sms_receive.epoch == 0
            {
                return Err(ActorError::new(
                    "sms_receive_session_changed",
                    "stored SMS work belongs to a retired receive session",
                ));
            }
        }
        if (self.native.managed()
            && (!self.native.ready || self.native.refreshing || !self.strict_sms_ready()))
            || !self.initialization.sms_ready()
        {
            return Err(ActorError::new(
                "sms_not_ready",
                "stored SMS requires completed initialization and confirmed PDU mode",
            ));
        }
        Ok(())
    }

    pub(super) fn configure_receive(&mut self, enabled: bool) -> Result<(), ActorError> {
        if self.port.is_none() {
            return Err(ActorError::unavailable("serial port is not open"));
        }
        if self.sms_receive.epoch == 0 {
            return Err(ActorError::unavailable("SMS receive epoch exhausted"));
        }
        let initial = !self.sms_receive.configured;
        self.sms_receive.configured = true;
        if self.sms_receive.enabled == enabled {
            return Ok(());
        }
        self.sms_receive.invalidate();
        if self.sms_receive.epoch == 0 {
            return Err(ActorError::unavailable("SMS receive epoch exhausted"));
        }
        self.sms_receive.enabled = enabled;
        if enabled && initial {
            // Open may receive frames before the authenticated open response.
            // Adopt only undelivered notifications; no Go reader exists yet.
            for frame in self.frames.iter_mut().chain(self.command_events.iter_mut()) {
                if !frame.sms_ingest_owned {
                    frame.sms_ingest_owned = self.sms_receive.observe(
                        self.serial_session,
                        frame.sms_generation,
                        frame.sms_event.as_ref(),
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn scan_receive(&mut self) -> Result<(), ActorError> {
        if self.port.is_none() {
            return Err(ActorError::unavailable("serial port is not open"));
        }
        if self.initialization.cached.is_some() && !self.initialization.sms_ready() {
            return Err(ActorError::new(
                "sms_not_ready",
                "stored SMS initialization failed; source records retained",
            ));
        }
        // disableURCRead/custom callbacks apply only to automatic CMTI. An
        // explicit refresh or initialization-complete scan remains available.
        self.sms_receive
            .push(self.serial_session, self.sms_generation, None, None)
    }

    pub(super) fn next_receive(&mut self) -> Option<ReceiveWork> {
        if self.native.managed()
            && (!self.native.ready || self.native.refreshing || !self.strict_sms_ready())
        {
            return None;
        }
        if !self.initialization.sms_ready() {
            return None;
        }
        if let Some(active) = &self.sms_receive.active {
            return Some(active.clone());
        }
        let job = self.sms_receive.pending.pop_front()?;
        self.sms_receive.active = Some(job.clone());
        Some(job)
    }

    pub(super) fn finish_receive(&mut self, id: u64) {
        if self
            .sms_receive
            .active
            .as_ref()
            .is_some_and(|job| job.id == id)
        {
            self.sms_receive.active = None;
        }
    }
}

#[cfg(test)]
mod tests {
    include!("sms_receive_tests.rs");
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReceiveControlRequest {
    pub(super) action: String,
    pub(super) enabled: Option<bool>,
}
