//! Native facade for the public N-wait runtime boundary. All card discovery,
//! durable identity/recovery policy and live execution stay in Rust.
pub(crate) use crate::multistandby_native_runtime::{
    Action, DueDelivery, Resources, SendReadiness,
};
use crate::multistandby_native_runtime::{Inventory, Manager, ProfileSpec};
use crate::multistandby_runtime_report::{RegistrationDue, RetryDue};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, oneshot};
mod inventory;

#[derive(Debug, Clone)]
pub(crate) struct Failure {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}
impl Failure {
    fn code(code: &'static str) -> Self {
        let message = match code {
            "native_multi_stopping" => "正在退出，已取消多待操作",
            "native_multi_reader_unavailable" => "请先连接读卡器",
            "native_multi_inventory_stale" => "读卡器选择已变化，请刷新卡片清单",
            "native_multi_inventory_invalid" => "卡片清单无效，请刷新后重试",
            "native_multi_store_unavailable" => "多待存储暂不可用",
            "native_multi_cleanup_unconfirmed" => "多待读卡器资源释放尚未确认",
            "profile_not_found" => "所选Profile不属于当前卡片或已不可用",
            "native_multi_queue_full" => "多待请求繁忙，请稍后重试",
            _ => "多待操作暂未完成，请检查线路状态后重试",
        };
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Failure {}
impl From<&'static str> for Failure {
    fn from(code: &'static str) -> Self {
        Self::code(code)
    }
}

#[derive(Default)]
struct Cache {
    inventory: Option<Inventory>,
    read_at: Option<Instant>,
    catalog: Option<Value>,
    card_profiles: Vec<Value>,
    recovered: Option<(String, u64)>,
}
pub(crate) struct Service {
    manager: Arc<Manager>,
    owner: crate::reader_pcsc::Owner,
    gate: Arc<crate::reader_reservation::Gate>,
    card: crate::multistandby_card::Service,
    store: crate::multistandby_store::Store,
    cache: Mutex<Cache>,
    serial: tokio::sync::Mutex<()>,
    slots: Arc<Semaphore>,
    stopping: AtomicBool,
}
impl Service {
    /// Production supplies default ConnectionOptions, matching the original
    /// N-wait empty override request. Every native preparation reads current
    /// carrier overrides and automatic proxy selection afresh. Tests may use
    /// explicit loopback connection options through the same typed resources.
    pub(crate) fn new(resources: Resources) -> Result<Arc<Self>, Failure> {
        let owner = resources.owner.clone();
        let gate = resources.gate.clone();
        let card = resources.card_service.clone();
        let store = resources.store.clone();
        let manager = Manager::new(resources).map_err(Failure::from)?;
        Ok(Arc::new(Self {
            manager,
            owner,
            gate,
            card,
            store,
            cache: Mutex::new(Cache::default()),
            serial: tokio::sync::Mutex::new(()),
            slots: Arc::new(Semaphore::new(32)),
            stopping: AtomicBool::new(false),
        }))
    }
    pub(crate) fn boot_id(&self) -> &str {
        self.manager.boot_id()
    }
    fn admit(&self) -> Result<tokio::sync::OwnedSemaphorePermit, Failure> {
        if self.stopping.load(Ordering::Acquire) {
            return Err("native_multi_stopping".into());
        }
        self.slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "native_multi_queue_full".into())
    }
    /// This is an internal typed boundary, not another HTTP/RPC dispatcher.
    /// Refresh returns discovery facts for the existing public card projection;
    /// a normal snapshot returns the live facts for the existing profile join.
    pub(crate) async fn snapshot(
        self: &Arc<Self>,
        card_id: &str,
        refresh_inventory: bool,
        recovery_probe: bool,
    ) -> Result<Value, Failure> {
        if card_id.len() > 512 || card_id.contains('\0') || (recovery_probe && !refresh_inventory) {
            return Err("invalid_request".into());
        }
        if !refresh_inventory {
            if self.stopping.load(Ordering::Acquire) {
                return Err("native_multi_stopping".into());
            }
            return Ok(self.manager.snapshot(card_id));
        }
        let permit = self.admit()?;
        let service = self.clone();
        let (send, wait) = oneshot::channel();
        // A dropped HTTP waiter cannot detach an in-flight physical inventory
        // from shutdown. The serial guard covers physical completion and commit.
        tokio::spawn(async move {
            let _permit = permit;
            let _serial = service.serial.lock().await;
            if service.stopping.load(Ordering::Acquire) {
                let _ = send.send(Err("native_multi_stopping".into()));
                return;
            }
            let result = service.refresh_inventory().await;
            let result = match result {
                Ok(()) if !send.is_closed() && !service.stopping.load(Ordering::Acquire) => {
                    service.recover_once().await
                }
                result => result,
            };
            let profiles = service.cache.lock().unwrap().card_profiles.clone();
            let detail = result
                .as_ref()
                .err()
                .map(|e| e.message.as_str())
                .unwrap_or("");
            let value = json!({"card_profiles":profiles,"boot_id":service.boot_id(),"available":result.is_ok(),"detail":detail});
            let _ = send.send(Ok(value));
        });
        wait.await
            .map_err(|_| Failure::from("native_multi_snapshot_unconfirmed"))?
    }
    /// The public operations owner claims its durable operation before calling
    /// this method and records the returned result. No command is replayed here.
    pub(crate) async fn execute(
        self: &Arc<Self>,
        action: Action,
        card_id: String,
        profile_ids: Vec<String>,
    ) -> Result<Value, Failure> {
        let permit = self.admit()?;
        let service = self.clone();
        let (send, wait) = oneshot::channel();
        tokio::spawn(async move {
            let _permit = permit;
            let _serial = service.serial.lock().await;
            let result = if service.stopping.load(Ordering::Acquire) {
                Err("native_multi_stopping".into())
            } else {
                match service.manager.execute(action, card_id, profile_ids) {
                    Ok(ticket) => ticket.wait().await.map_err(Failure::from),
                    Err(error) => Err(error.into()),
                }
            };
            let _ = send.send(result);
        });
        wait.await
            .map_err(|_| Failure::from("native_multi_command_unconfirmed"))?
    }
    pub(crate) fn ready(&self, profile_id: &str) -> SendReadiness {
        if self.stopping.load(Ordering::Acquire) {
            return SendReadiness {
                session_current: false,
                channel_ready: false,
            };
        }
        self.manager.send_readiness(profile_id)
    }
    pub(crate) fn maintenance(&self) -> bool {
        !self.stopping.load(Ordering::Acquire) && self.manager.maintenance()
    }
    pub(crate) fn dispatch_retry_due(&self, due: RetryDue) -> DueDelivery {
        if self.stopping.load(Ordering::Acquire) {
            DueDelivery::Stale
        } else {
            self.manager.dispatch_retry_due(due)
        }
    }
    pub(crate) fn dispatch_registration_due(&self, due: RegistrationDue) -> DueDelivery {
        if self.stopping.load(Ordering::Acquire) {
            DueDelivery::Stale
        } else {
            self.manager.dispatch_registration_due(due)
        }
    }
    pub(crate) fn retry_due(&self, due: RetryDue) -> bool {
        !self.stopping.load(Ordering::Acquire) && self.manager.retry_due(due)
    }
    pub(crate) fn registration_due(&self, due: RegistrationDue) -> bool {
        !self.stopping.load(Ordering::Acquire) && self.manager.registration_due(due)
    }
    /// Used before physical selection changes. Unlike application shutdown this
    /// preserves desired state and permits a later, freshly enumerated generation.
    pub(crate) async fn stop_and_drain(
        self: &Arc<Self>,
        reader: &str,
        generation: u64,
    ) -> Result<Value, Failure> {
        self.drain_generation(reader.to_owned(), generation).await
    }
    pub(crate) async fn retire_generation(
        self: &Arc<Self>,
        reader: &str,
        generation: u64,
    ) -> Result<Value, Failure> {
        self.drain_generation(reader.to_owned(), generation).await
    }
    async fn drain_generation(
        self: &Arc<Self>,
        reader: String,
        generation: u64,
    ) -> Result<Value, Failure> {
        let service = self.clone();
        let (send, wait) = oneshot::channel();
        tokio::spawn(async move {
            let _serial = service.serial.lock().await;
            let result = match service.manager.reader_lost(reader.clone(), generation) {
                Ok(ticket) => ticket.wait().await.map_err(Failure::from),
                Err(error) => Err(error.into()),
            };
            if result.is_ok() {
                let mut cache = service.cache.lock().unwrap();
                if cache
                    .inventory
                    .as_ref()
                    .is_some_and(|i| i.reader == reader && i.generation == generation)
                {
                    cache.inventory = None;
                    cache.read_at = None;
                    cache.catalog = None;
                    cache.recovered = None;
                }
            }
            let _ = send.send(result);
        });
        wait.await
            .map_err(|_| Failure::from("native_multi_cleanup_unconfirmed"))?
    }
    pub(crate) fn begin_shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        self.manager.begin_shutdown();
    }
    pub(crate) async fn shutdown(self: &Arc<Self>) -> Result<(), Failure> {
        self.begin_shutdown();
        let service = self.clone();
        let (send, wait) = oneshot::channel();
        tokio::spawn(async move {
            let _serial = service.serial.lock().await;
            let result = service.manager.shutdown().await.map_err(Failure::from);
            let _ = send.send(result);
        });
        wait.await
            .map_err(|_| Failure::from("native_multi_cleanup_unconfirmed"))?
    }
}

#[cfg(test)]
mod tests;
