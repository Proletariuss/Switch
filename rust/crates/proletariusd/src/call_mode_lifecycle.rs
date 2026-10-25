//! Authoritative module call-mode lifecycle. Accepted operations are detached
//! from HTTP waiters and retain their radio reservation through real cleanup.
use crate::call_mode_restore_journal::{
    Intent as RestoreIntent, Journal as RestoreJournal, Receipt as RestoreReceipt,
};
use crate::call_mode_state::{Operation, State, Status};
use crate::module_radio::{CallModeReservation, Service as Radio};
use crate::module_radio_transport::{Action, Outcome, UsbComposition, VoiceConfiguration};
use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, Notify, oneshot};

pub(crate) const PREFIX: &str = "/api/internal/call-mode/lifecycle/";
const RECONNECT_BUDGET: Duration = Duration::from_secs(90);
const ADB_SETTLE: Duration = Duration::from_secs(35);
const PRISTINE_MARKER: &str = ".call-mode-pristine-v1";

fn absent(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

pub(crate) fn pristine_marker(root: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(root.join(PRISTINE_MARKER)) {
        Ok(metadata) if metadata.is_file() && metadata.len() == 0 && metadata.nlink() == 1 => {
            Ok(true)
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "call-mode pristine marker is invalid",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Only a genuinely empty root may establish the read-only baseline. Legacy
/// lock files remain ambiguous and are never silently reclassified as clean.
fn initialize_pristine_marker(root: &Path) {
    if ![
        "call-mode-operation.lock",
        ".module-adb-interface.lock",
        "module-backups",
        "voice-runtime",
        "module-legacy-locks-v1",
        crate::call_mode_restore_journal::ACTIVE_NAME,
        crate::call_mode_restore_journal::ARCHIVE_NAME,
    ]
    .iter()
    .all(|name| absent(&root.join(name)))
        || !matches!(pristine_marker(root), Ok(false))
    {
        return;
    }
    if let Ok(file) = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(root.join(PRISTINE_MARKER))
    {
        let _ = file.sync_all();
    }
}

/// Revoke before the first physical mutation. A crash then leaves the lock
/// files as conservative evidence for the next daemon.
pub(crate) fn invalidate_pristine_marker(root: &Path) -> io::Result<()> {
    crate::module_recovery::legacy_locks::invalidate(root)?;
    if pristine_marker(root)? {
        fs::remove_file(root.join(PRISTINE_MARKER))?;
        fs::File::open(root)?.sync_all()?;
    }
    Ok(())
}
#[derive(Clone)]
pub(crate) struct Service(Arc<Inner>);
struct Inner {
    root: PathBuf,
    state: State,
    radio: Radio,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    admission: Mutex<()>,
    refresh: Mutex<()>,
    active_observation: StdMutex<Option<ActiveObservation>>,
    #[cfg(test)]
    refresh_probe: StdMutex<Option<crate::call_mode_adb::child_tests::HeldProbe>>,
    stopping: AtomicBool,
    operation_started: AtomicBool,
    cancelled_producer: StdMutex<Option<String>>,
    status_producer: StdMutex<String>,
    active_reads: AtomicUsize,
    shutdown_gate: Mutex<()>,
    shutdown_receipt: StdMutex<Option<Result<Vec<String>, String>>>,
    active_restore: StdMutex<Option<ActiveRestore>>,
    finished: Notify,
}
struct ActiveObservation {
    root: PathBuf,
    producer: String,
    token: Arc<()>,
}
struct ObservationOwner {
    inner: Arc<Inner>,
    token: Arc<()>,
}
impl Drop for ObservationOwner {
    fn drop(&mut self) {
        let mut active = self
            .inner
            .active_observation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if active
            .as_ref()
            .is_some_and(|entry| Arc::ptr_eq(&entry.token, &self.token))
        {
            *active = None;
        }
    }
}

enum Job {
    Enable(bool),
    Restore(crate::call_mode_backups::CapturedNativeBackup),
    Download(String),
    Prepare(String, String),
}
/// A verified target is not yet a completed operation. Keep its exact input
/// alive until the original radio reservation acknowledges actual release.
/// The pending result retains its durable journal but grants no new I/O rights.
struct PreparedRestore {
    captured: crate::call_mode_backups::CapturedNativeBackup,
    receipt: VerifiedRestore,
}
struct VerifiedRestore {
    status: Status,
    namespace: crate::call_mode_backups::OperationNamespaceWitness,
    journal: RestoreJournal,
    durable_receipt: RestoreReceipt,
}
struct ActiveRestore {
    operation: Operation,
    journal: RestoreJournal,
    admission: crate::module_recovery::RestoreJournalAdmission,
}
#[derive(Debug)]
struct Failure {
    summary: String,
    detail: String,
    backup: String,
}
impl Failure {
    fn new(summary: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            detail: detail.into(),
            backup: String::new(),
        }
    }
    fn backup(mut self, path: &str) -> Self {
        self.backup = path.into();
        self
    }
}
type Reply = Response<Full<Bytes>>;
fn failure(status: StatusCode, error: impl AsRef<str>) -> Reply {
    crate::json_response(
        status,
        &json!({"error":error.as_ref(),"code":"call_mode_lifecycle_unconfirmed"}),
    )
}
fn ready_voice(voice: VoiceConfiguration) -> bool {
    voice.ims == 1 && voice.volte_capability == 1 && voice.volte_disabled == 0
}
fn same_voice(a: VoiceConfiguration, b: VoiceConfiguration) -> bool {
    a.ims == b.ims && a.volte_disabled == b.volte_disabled
}
fn timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

impl Service {
    pub(crate) fn new(
        root: &Path,
        radio: Radio,
        inbox: crate::message_archive::InboxState,
        usb_at: crate::usb_at::Owner,
    ) -> Self {
        if !radio.module_recovery().is_quarantined() {
            initialize_pristine_marker(root);
        }
        Self(Arc::new(Inner {
            root: root.into(),
            state: State::new(root),
            radio,
            inbox,
            usb_at,
            admission: Mutex::new(()),
            refresh: Mutex::new(()),
            active_observation: StdMutex::new(None),
            #[cfg(test)]
            refresh_probe: StdMutex::new(None),
            stopping: AtomicBool::new(false),
            operation_started: AtomicBool::new(false),
            cancelled_producer: StdMutex::new(None),
            status_producer: StdMutex::new(String::new()),
            active_reads: AtomicUsize::new(0),
            shutdown_gate: Mutex::new(()),
            shutdown_receipt: StdMutex::new(None),
            active_restore: StdMutex::new(None),
            finished: Notify::new(),
        }))
    }
    fn current(&self, op: &Operation) -> Result<(), Failure> {
        if !self.0.state.is_current(op) || !self.producer_current(&op.producer_id) {
            return Err(Failure::new(
                "通话模式后端代次已变化",
                "旧操作已停止，不能向新的模块连接提交或发布结果",
            ));
        }
        if self.0.stopping.load(Ordering::Acquire) {
            return Err(Failure::new(
                "通话模式操作已停止",
                "服务正在退出，已停止后续配置与部署，正在等待实际清理",
            ));
        }
        self.0
            .radio
            .module_recovery()
            .root_proof_for(&self.0.root)
            .map_err(|()| {
                Failure::new(
                    "通话模式数据目录归属已变化",
                    "原后端的目录锁已失效，未继续配置写入或发布成功回执",
                )
            })?;
        Ok(())
    }
    fn publish(&self, op: &Operation, status: Status) -> Result<(), Failure> {
        self.current(op)?;
        if !self.0.state.publish(op, status) {
            return Err(Failure::new("通话模式操作已过期", "状态代次已变化"));
        }
        Ok(())
    }
    fn backup_path(&self, id: &str) -> String {
        self.0
            .root
            .join("module-backups")
            .join(id)
            .to_string_lossy()
            .into_owned()
    }
    pub(crate) fn public_receipt_scope(&self) -> (PathBuf, String) {
        let revision = self.0.state.operation_generation();
        (
            self.0.root.clone(),
            format!(
                "{}:lifecycle:{}",
                self.0.radio.public_mutation_binding(),
                revision
            ),
        )
    }
    pub(crate) async fn start_public_enable(&self, confirm_adb: bool, binding: String) -> Reply {
        self.start_bound(
            Job::Enable(confirm_adb),
            Status::new("enabling_usb", "正在读取并备份模块 USB 配置"),
            Some(binding),
        )
        .await
    }
    pub(crate) async fn start_public_restore(&self, id: String, binding: String) -> Reply {
        self.start_restore_bound(id, Some(binding)).await
    }
    pub(crate) async fn start_enable(&self, confirm_adb: bool) -> Reply {
        self.start(
            Job::Enable(confirm_adb),
            Status::new("enabling_usb", "正在读取并备份模块 USB 配置"),
        )
        .await
    }
    pub(crate) async fn start_restore(&self, id: String) -> Reply {
        self.start_restore_bound(id, None).await
    }
    async fn start_restore_bound(&self, id: String, binding: Option<String>) -> Reply {
        let proof = match self.0.radio.module_recovery().root_proof_for(&self.0.root) {
            Ok(proof) => proof,
            Err(()) => {
                return failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "当前后端的数据目录归属未能确认，未提交还原操作",
                );
            }
        };
        let captured = match crate::call_mode_backups::capture_native_backup(
            &self.0.root,
            &id,
            &proof,
        )
        .await
        {
            Ok(captured) => captured,
            Err(error) => return failure(StatusCode::BAD_REQUEST, error),
        };
        let mut status = Status::new("restoring_usb", "正在核对备份与当前模块");
        status.backup_path = self.backup_path(&id);
        self.start_bound(Job::Restore(captured), status, binding)
            .await
    }
    async fn verify_restore_input(
        &self,
        captured: &crate::call_mode_backups::CapturedNativeBackup,
    ) -> Result<(), String> {
        let proof = self
            .0
            .radio
            .module_recovery()
            .root_proof_for(&self.0.root)
            .map_err(|()| "当前后端的数据目录归属已变化，未确认还原输入".to_owned())?;
        captured.verify_for_root(&self.0.root, &proof).await
    }
    pub(crate) async fn start_download(&self, source: String) -> Reply {
        self.start(Job::Download(source.clone()), download_status(&source))
            .await
    }
    async fn start(&self, job: Job, status: Status) -> Reply {
        self.start_bound(job, status, None).await
    }
    async fn start_bound(
        &self,
        job: Job,
        status: Status,
        expected_binding: Option<String>,
    ) -> Reply {
        if self.0.radio.module_recovery().is_admission_revoked() {
            return crate::module_recovery::rejection();
        }
        // Admission is itself an owner task: disconnecting before the 202
        // response cannot orphan a reservation acquired in the meantime.
        let service = self.clone();
        let admission_pin = ReadPin::new(self.0.clone());
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let _pin = admission_pin;
            service
                .admit_bound(job, status, send, expected_binding)
                .await;
        });
        receive.await.unwrap_or_else(|_| {
            failure(StatusCode::SERVICE_UNAVAILABLE, "通话模式操作准入未能确认")
        })
    }
    #[cfg(test)]
    async fn admit(&self, job: Job, status: Status, send: oneshot::Sender<Reply>) {
        self.admit_bound(job, status, send, None).await;
    }
    async fn admit_bound(
        &self,
        job: Job,
        status: Status,
        send: oneshot::Sender<Reply>,
        expected_binding: Option<String>,
    ) {
        let Ok(gate) = self.0.admission.try_lock() else {
            let _ = send.send(failure(StatusCode::CONFLICT, "通话模式正在执行其他操作"));
            return;
        };
        if self.0.stopping.load(Ordering::Acquire) || self.0.state.running() {
            let _ = send.send(failure(StatusCode::CONFLICT, "通话模式正在执行其他操作"));
            return;
        }
        if let Job::Restore(captured) = &job {
            if let Err(error) = self.verify_restore_input(captured).await {
                let _ = send.send(failure(StatusCode::BAD_REQUEST, error));
                return;
            }
        }
        let reservation = if matches!(&job, Job::Restore(_)) {
            self.0.radio.reserve_call_mode_restore_operation().await
        } else {
            self.0.radio.reserve_call_mode_operation().await
        };
        let lease = match reservation {
            Ok(lease) => lease,
            Err(error) => {
                let _ = send.send(failure(error.public_status(), error.public_message()));
                return;
            }
        };
        if !self.producer_current(&lease.producer_id) || self.0.stopping.load(Ordering::Acquire) {
            self.release_until_confirmed(&lease, None).await;
            let _ = send.send(failure(StatusCode::CONFLICT, "后端代次已变化或正在退出"));
            return;
        }
        if let Err(error) = self.0.radio.call_mode_ensure_idle(&lease).await {
            let _ = send.send(failure(StatusCode::CONFLICT, error.public_message()));
            self.release_until_confirmed(&lease, None).await;
            return;
        }
        if matches!(&job, Job::Download(_)) {
            let (revision, _) = self.0.state.observation();
            let inspected = crate::call_mode_status::inspect_with_reservation(
                &self.0.root,
                &self.0.radio,
                &lease,
                &lease.producer_id,
                self.0.inbox.clone(),
            )
            .await;
            if inspected.state != "needs_download" {
                self.0
                    .state
                    .publish_observation(revision, inspected.clone());
                let detail = format!("{}：{}", inspected.summary, inspected.detail);
                let _ = send.send(failure(StatusCode::CONFLICT, detail));
                self.release_until_confirmed(&lease, None).await;
                return;
            }
        }
        let physical_change = !matches!(&job, Job::Download(_));
        if physical_change && invalidate_pristine_marker(&self.0.root).is_err() {
            self.release_until_confirmed(&lease, None).await;
            let _ = send.send(failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "无法记录模块通话模式操作，未提交设备修改",
            ));
            return;
        }
        if expected_binding
            .as_ref()
            .is_some_and(|binding| self.public_receipt_scope().1 != *binding)
        {
            self.release_until_confirmed(&lease, None).await;
            let _ = send.send(failure(
                StatusCode::CONFLICT,
                "原请求的模块或通话模式代次已变化",
            ));
            return;
        }
        let Some(op) = self.0.state.begin(&lease.producer_id, status) else {
            self.release_until_confirmed(&lease, None).await;
            let _ = send.send(failure(StatusCode::CONFLICT, "通话模式正在执行其他操作"));
            return;
        };
        if physical_change {
            self.0.operation_started.store(true, Ordering::Release);
        }
        *self
            .0
            .status_producer
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = lease.producer_id.clone();
        let _ = send.send(crate::json_response(
            StatusCode::ACCEPTED,
            &self.0.state.snapshot(),
        ));
        drop(gate);
        // A separate retained join allows the outer owner to release even when
        // an unexpected panic terminates business sequencing.
        let owner = self.clone();
        let worker_op = op.clone();
        let worker_lease = lease.clone();
        let restore_attempt = matches!(&job, Job::Restore(_));
        let result =
            tokio::spawn(async move { owner.run(job, &worker_op, &worker_lease).await }).await;
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(_) => {
                self.fail(
                    &op,
                    Failure::new("通话模式操作未完成", "原生工作线程异常，正在确认资源释放"),
                )
                .await;
                None
            }
        };
        // Cleanup keeps the original identity and remains owned even after a
        // panic leaves a detached physical worker draining its borrowed pin.
        // Retrying release is idempotent; it never repeats configuration I/O.
        self.complete_operation(&op, &lease, prepared, restore_attempt)
            .await;
    }
    async fn complete_operation(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        prepared: Option<PreparedRestore>,
        restore_attempt: bool,
    ) {
        let unowned = self
            .0
            .active_restore
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none();
        if restore_attempt && unowned {
            // begin() can leave an Intent before it returns (for example a
            // failed fsync/panic). Such evidence has no registered live owner
            // and must block new business even in this same daemon lifetime.
            self.0
                .radio
                .module_recovery()
                .observe_unowned_restore_journal();
        }
        self.release_until_confirmed(lease, Some(op)).await;
        // This checkpoint describes the actual original release, including a
        // failed/panicked restore. It cannot turn an unresolved write into a
        // verified target or grant recovery rights on a later startup.
        let mut active = {
            let mut active = self
                .0
                .active_restore
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if active.as_ref().is_some_and(|owner| owner.operation == *op) {
                active.take()
            } else {
                None
            }
        };
        let released = active
            .as_ref()
            .map(|owner| owner.journal.released())
            .transpose();
        if let Err(error) = &released {
            self.fail(
                op,
                Failure::new("还原释放记录未能持久保存", error.to_string()),
            )
            .await;
        }
        if let Some(prepared) = prepared {
            let owned = active
                .as_ref()
                .is_some_and(|owner| owner.journal.same_instance(&prepared.receipt.journal));
            if !owned {
                self.fail(
                    op,
                    Failure::new(
                        "还原回执的持久记录归属未确认",
                        "未发布成功回执，保留恢复记录供核查",
                    ),
                )
                .await;
            } else if released.is_ok() {
                if let Err(error) = self
                    .publish_released_restore(
                        op,
                        prepared,
                        &mut active.as_mut().expect("checked exact owner").admission,
                    )
                    .await
                {
                    self.fail(op, error).await;
                }
            }
        }
        // Failed/unarchived owners atomically revoke future ordinary rights
        // before exposing an idle lifecycle. Existing exact cleanup has ended.
        drop(active);
        self.0.state.end(op);
        self.0.finished.notify_waiters();
    }
    async fn publish_released_restore(
        &self,
        op: &Operation,
        mut prepared: PreparedRestore,
        admission: &mut crate::module_recovery::RestoreJournalAdmission,
    ) -> Result<(), Failure> {
        self.current(op)?;
        self.verify_restore_input(&prepared.captured)
            .await
            .map_err(|e| Failure::new("还原回执的输入归属未确认", e))?;
        prepared
            .receipt
            .namespace
            .validate()
            .map_err(|e| Failure::new("还原回执的原操作锁路径已变化", e.to_string()))?;
        // The timestamp describes acknowledged settlement, not the earlier
        // target readback. No lease or released lock becomes new I/O authority.
        prepared.receipt.durable_receipt.restored_at = timestamp();
        prepared
            .receipt
            .journal
            .finish_after_release(prepared.receipt.durable_receipt.clone())
            .map_err(|e| Failure::new("还原成功回执未能持久保存", e.to_string()))?;
        self.verify_restore_input(&prepared.captured)
            .await
            .map_err(|e| Failure::new("还原回执的输入归属未确认", e))?;
        prepared
            .receipt
            .namespace
            .validate()
            .map_err(|e| Failure::new("还原回执的原操作锁路径已变化", e.to_string()))?;
        self.current(op)?;
        admission.complete().map_err(|()| {
            Failure::new(
                "还原持久回执的目录归属未确认",
                "未开放新的模块操作或发布成功回执",
            )
        })?;
        if let Some(receipt) = prepared.receipt.status.last_restore.as_mut() {
            receipt["restored_at"] = json!(prepared.receipt.durable_receipt.restored_at);
        }
        self.publish(op, prepared.receipt.status)
    }

    async fn begin_restore_journal(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        captured: &crate::call_mode_backups::CapturedNativeBackup,
        module_imei: &str,
        current_usb: UsbComposition,
        current_voice: VoiceConfiguration,
    ) -> Result<RestoreJournal, Failure> {
        self.current(op)?;
        self.0
            .radio
            .validate_call_mode_operation_lock(lease)
            .map_err(|e| Failure::new("还原操作锁归属未确认", e.public_message()))?;
        let proof = self
            .0
            .radio
            .module_recovery()
            .root_proof_for(&self.0.root)
            .map_err(|()| Failure::new("无法记录还原意图", "数据目录锁归属未确认"))?;
        let backup = captured.backup();
        // Capture through this already accepted finite owner before any
        // route cleanup or persistent configuration write. A serial/unbound
        // transport is recorded as such; a native USB capture error cannot
        // become a guessed IMEI/VID/PID recovery target.
        let recovery_binding = self
            .0
            .radio
            .capture_restore_physical_binding(lease)
            .await
            .map_err(|e| Failure::new("无法捕获还原设备的物理绑定", e.public_message()))?;
        self.current(op)?;
        // Keep the actual ADB admission drained until the observed original
        // obligation and the new restore admission fence are both durable.
        // Drop before any ordinary route settlement, which acquires ADB again.
        let voice_capture = crate::call_mode_adb::capture_restore_voice_binding(
            &self.0.root,
            self.0.inbox.clone(),
            self.0.usb_at.clone(),
            self.0.radio.clone(),
            lease,
        )
        .await
        .map_err(|e| Failure::new("无法捕获原语音清理义务", e.to_string()))?;
        self.current(op)?;
        voice_capture
            .validate_for_publication()
            .map_err(|e| Failure::new("原语音观察的归属已变化", e.to_string()))?;
        let journal = RestoreJournal::begin(
            &self.0.root,
            &proof,
            captured,
            RestoreIntent {
                producer_id: lease.producer_id.clone(),
                reservation_token: lease.token.clone(),
                reservation_sequence: lease.sequence,
                operation_id: format!("restore-{}", op.generation),
                module_imei: module_imei.into(),
                current_usb,
                current_voice,
                target_usb: backup.usb,
                target_voice: backup.voice,
                recovery_binding,
                voice_recovery_binding: Some(voice_capture.binding().clone()),
            },
        )
        .await
        .map_err(|e| Failure::new("无法持久记录还原意图", e.to_string()))?;
        let admission = self
            .0
            .radio
            .module_recovery()
            .start_restore_journal(&journal)
            .map_err(|()| {
                Failure::new(
                    "无法保留还原期间的新操作隔离",
                    "未提交模块副作用，恢复记录保留供核查",
                )
            })?;
        {
            let mut active = self
                .0
                .active_restore
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if active.is_some() {
                return Err(Failure::new(
                    "还原记录仍有待核对操作",
                    "未替换原操作或提交新的模块写入",
                ));
            }
            *active = Some(ActiveRestore {
                operation: op.clone(),
                journal: journal.clone(),
                admission,
            });
        }
        self.current(op)?;
        self.0
            .radio
            .attach_restore_journal(lease, journal.clone())
            .map_err(|e| Failure::new("还原记录与原模块预约未能绑定", e.public_message()))?;
        voice_capture
            .validate_for_publication()
            .map_err(|e| Failure::new("原语音观察的归属已变化", e.to_string()))?;
        drop(voice_capture);
        Ok(journal)
    }

    fn restore_journal(&self, op: &Operation) -> Result<RestoreJournal, Failure> {
        self.0
            .active_restore
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|owner| owner.operation == *op)
            .map(|owner| owner.journal.clone())
            .ok_or_else(|| Failure::new("还原持久记录未确认", "未发布成功回执或继续模块写入"))
    }
    // All release paths keep their admission/read pin until real release is
    // acknowledged. A transient full radio queue must not orphan ownership.
    async fn release_until_confirmed(&self, lease: &CallModeReservation, op: Option<&Operation>) {
        loop {
            match self.0.radio.release_call_mode_operation(lease).await {
                Ok(()) => return,
                Err(error) => {
                    if let Some(op) = op {
                        self.fail(
                            op,
                            Failure::new("射频授权释放未确认", error.public_message()),
                        )
                        .await;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    async fn run(
        &self,
        job: Job,
        op: &Operation,
        lease: &CallModeReservation,
    ) -> Option<PreparedRestore> {
        let result = match job {
            Job::Enable(confirm) => self.enable(op, lease, confirm).await.map(|()| None),
            Job::Restore(captured) => self
                .restore(op, lease, &captured)
                .await
                .map(|receipt| Some(PreparedRestore { captured, receipt })),
            Job::Download(source) => self.download(op, lease, &source).await.map(|()| None),
            Job::Prepare(backup, source) => self
                .prepare(op, lease, &backup, &source)
                .await
                .map(|()| None),
        };
        match result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail(op, error).await;
                None
            }
        }
    }
    async fn fail(&self, op: &Operation, error: Failure) {
        if !self.0.inbox.owns_module_producer(&op.producer_id) || !self.0.state.is_current(op) {
            return;
        }
        let old = self.0.state.snapshot();
        let mut status = Status::new("failed", &error.summary);
        status.detail = error.detail;
        status.can_retry = true;
        status.adb_authorization_accepted = old.adb_authorization_accepted;
        if status.adb_authorization_accepted {
            status.detail = format!(
                "模块已接受 ADB 持久授权；USB 配置备份只能关闭接口，不能保证撤销授权。\n{}",
                status.detail
            );
        }
        status.backup_path = if error.backup.is_empty() {
            old.backup_path
        } else {
            error.backup
        };
        status.adb_enabled = old.adb_enabled;
        status.uac_enabled = old.uac_enabled;
        status.interfaces_ready = old.interfaces_ready;
        status.voice_configured = old.voice_configured;
        status.source = old.source;
        let runtime = crate::call_mode_runtime::installed_runtime_verification(&self.0.root).await;
        status.runtime_downloaded = runtime.runtime_downloaded;
        status.downloaded_bytes = runtime.downloaded_bytes;
        self.0.state.publish(op, status);
    }
    async fn action(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        action: Action,
        summary: &str,
    ) -> Result<Outcome, Failure> {
        self.current(op)?;
        let result = self
            .0
            .radio
            .call_mode_action(lease, action)
            .await
            .map_err(|error| Failure::new(summary, error.public_message()))?;
        self.current(op)?;
        if !result.complete || !result.error_code.is_empty() || result.uncertain {
            return Err(Failure::new(summary, action_detail(&result)));
        }
        Ok(result)
    }
    async fn usb(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
    ) -> Result<UsbComposition, Failure> {
        let value = self
            .action(
                op,
                lease,
                Action::CallModeUsbCompositionRead,
                "无法读取模块 USB 配置",
            )
            .await?;
        if !value.verified {
            return Err(Failure::new(
                "无法读取模块 USB 配置",
                "原生事务未返回已验证状态",
            ));
        }
        value
            .usb_composition
            .filter(|value| value.valid() && value.flags.len() == 7)
            .ok_or_else(|| Failure::new("无法识别当前 USB 配置", "原生事务未返回已验证的 USB 配置"))
    }
    async fn voice(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
    ) -> Result<VoiceConfiguration, Failure> {
        let value = self
            .action(
                op,
                lease,
                Action::CallModeVoiceRead,
                "无法确认模块 IMS/VoLTE 配置",
            )
            .await?;
        if !value.verified {
            return Err(Failure::new(
                "无法确认模块 IMS/VoLTE 配置",
                "原生事务未返回已验证状态",
            ));
        }
        value.voice.filter(|value| value.valid()).ok_or_else(|| {
            Failure::new(
                "无法确认模块 IMS/VoLTE 配置",
                "原生事务未返回已验证的 IMS/VoLTE 配置",
            )
        })
    }
    async fn idle(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        summary: &str,
        backup: &str,
    ) -> Result<(), Failure> {
        self.current(op)?;
        self.0
            .radio
            .call_mode_ensure_idle(lease)
            .await
            .map_err(|error| Failure::new(summary, error.public_message()).backup(backup))
    }
    async fn probe(&self, op: &Operation) -> Result<(), Failure> {
        self.current(op)?;
        crate::call_mode_adb::probe_module_adb(
            self.0.radio.module_recovery(),
            &self.0.root,
            &op.producer_id,
            self.0.inbox.clone(),
        )
        .await
        .map_err(|error| Failure::new("模块 ADB root 接口尚未就绪", error.to_string()))?;
        self.current(op)
    }
    async fn prepare(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        backup: &str,
        source: &str,
    ) -> Result<(), Failure> {
        let mut status = ready_status(
            "preparing",
            "正在部署并验证模块侧语音运行时",
            backup,
            source,
        );
        self.publish(op, status.clone())?;
        crate::call_mode_adb::prepare_native_runtime(
            &self.0.root,
            self.0.inbox.clone(),
            self.0.usb_at.clone(),
            &self.0.radio,
            lease,
        )
        .await
        .map_err(|(summary, detail)| Failure::new(summary, detail).backup(backup))?;
        status.state = "ready".into();
        status.summary = "通话模式已就绪".into();
        status.detail = "ADB、UAC、IMS/VoLTE、QDC507 驱动与 D4/UAC 路由启停自检均已通过。".into();
        self.publish(op, status)
    }
    async fn finish_enable(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        backup: &str,
    ) -> Result<(), Failure> {
        let runtime = crate::call_mode_runtime::installed_runtime_verification(&self.0.root).await;
        if !runtime.runtime_downloaded {
            let mut status = ready_status("needs_download", "需要下载模块侧语音运行时", backup, "");
            status.runtime_downloaded = false;
            status.downloaded_bytes = 0;
            status.can_download = true;
            status.requires_download_confirmation = true;
            status.detail="ADB、UAC 与 IMS/VoLTE 已完成真实验证。请选择下载渠道；下载后会自动校验、部署并执行一次语音路由自检。".into();
            return self.publish(op, status);
        }
        self.prepare(op, lease, backup, "").await
    }
    async fn download(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        source: &str,
    ) -> Result<(), Failure> {
        let observed = crate::call_mode_status::inspect_with_reservation(
            &self.0.root,
            &self.0.radio,
            lease,
            &op.producer_id,
            self.0.inbox.clone(),
        )
        .await;
        if !observed.interfaces_ready {
            return Err(Failure::new("模块通话接口尚未就绪", observed.detail));
        }
        let id = crate::call_mode_runtime_download::new_operation_id()
            .map_err(|_| Failure::new("无法创建运行时下载操作编号", "随机数服务不可用"))?;
        let current = self.clone();
        let current_op = op.clone();
        let progress = self.clone();
        let progress_op = op.clone();
        let source_owned = source.to_owned();
        crate::call_mode_runtime_download::run_native(
            &self.0.root,
            id,
            source.into(),
            Arc::new(move || current.current(&current_op).is_ok()),
            Arc::new(move |value| {
                let mut status = download_status(&source_owned);
                status.downloaded_bytes = value.downloaded_bytes;
                status.detail = value.detail;
                let _ = progress.publish(&progress_op, status);
            }),
        )
        .await
        .map_err(|error| Failure::new("模块语音运行时下载失败", error))?;
        self.current(op)?;
        self.prepare(op, lease, "", source).await
    }

    async fn enable(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        confirm_adb: bool,
    ) -> Result<(), Failure> {
        let original = self.usb(op, lease).await?;
        let target = original.call_mode_target();
        let original_voice = self.voice(op, lease).await?;
        let target_voice = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        let usb_change = original != target;
        let voice_change = !same_voice(original_voice, target_voice);
        let recovery = !usb_change && !voice_change;
        if recovery && self.probe(op).await.is_ok() {
            return self.finish_enable(op, lease, "").await;
        }
        let authorization = original.flags[5] == 0 || recovery;
        if authorization && !confirm_adb {
            return Err(Failure::new(
                "尚未确认模块 ADB 授权",
                "请在通话页面阅读授权影响并重新确认；模块配置尚未修改。",
            ));
        }
        self.current(op)?;
        let identity = self
            .0
            .radio
            .call_mode_identity(lease)
            .await
            .map_err(|e| Failure::new("无法确认当前模块身份", e.public_message()))?;
        let proof = self
            .0
            .radio
            .module_recovery()
            .root_proof_for(&self.0.root)
            .map_err(|()| Failure::new("无法保存通话配置备份", "数据目录锁归属未确认"))?;
        let backup_id = crate::call_mode_backups::create_native_backup(
            &self.0.root,
            identity.clone(),
            original,
            original_voice,
            false,
            &proof,
        )
        .await
        .map_err(|e| Failure::new("无法保存通话配置备份", e))?;
        let backup = self.backup_path(&backup_id);
        self.idle(op, lease, "通话模式准备已安全停止", &backup)
            .await?;
        if authorization {
            let mut status = Status::new("authorizing_adb", "正在持久授权模块 ADB");
            status.adb_authorization_required = true;
            status.uac_enabled = original.flags[6] == 1;
            status.backup_path = backup.clone();
            status.detail = "授权密码仅用于当前 AT 交互，不会写入日志、状态或备份文件。".into();
            self.publish(op, status)?;
            let result = self
                .action(
                    op,
                    lease,
                    Action::CallModeAdbAuthorize,
                    "模块 ADB 持久授权未通过",
                )
                .await
                .map_err(|e| e.backup(&backup))?;
            if !result.submitted || !result.verified {
                return Err(Failure::new(
                    "模块 ADB 持久授权未通过",
                    "授权结果未确认；模块可能已接受授权，请勿盲目重放",
                )
                .backup(&backup));
            }
        }
        let mut status = Status::new("enabling_usb", "正在准备模块通话接口");
        status.backup_path = backup.clone();
        status.adb_authorization_accepted = authorization;
        status.adb_enabled = true;
        status.uac_enabled = true;
        status.voice_configured = !voice_change;
        if usb_change {
            status.detail=if authorization{"模块已接受 ADB 持久授权；正在仅修改 ADB/UAC 功能位，并保留当前 VID/PID 与其余功能位。"}else{"正在仅修改缺少的 ADB/UAC 功能位，并保留当前 VID/PID 与其余功能位。"}.into();
            self.publish(op, status.clone())?;
            self.idle(op, lease, "通话模式准备已安全停止", &backup)
                .await?;
            let result = self
                .action(
                    op,
                    lease,
                    Action::CallModeUsbCompositionWrite {
                        expected: original,
                        target,
                    },
                    "USB 配置写入或回读未通过，模块尚未重启",
                )
                .await
                .map_err(|e| e.backup(&backup))?;
            if !result.verified {
                return Err(Failure::new(
                    "USB 配置写入或回读未通过，模块尚未重启",
                    action_detail(&result),
                )
                .backup(&backup));
            }
        }
        if voice_change {
            status.state = "enabling_voice".into();
            status.summary = "正在启用模块 IMS/VoLTE".into();
            status.detail = "正在启用 IMS 并确认 VoLTE 未被禁用；写入后会完整回读。".into();
            self.publish(op, status.clone())?;
            self.idle(op, lease, "通话模式准备已安全停止", &backup)
                .await?;
            let result = self
                .action(
                    op,
                    lease,
                    Action::CallModeVoice {
                        target: target_voice,
                    },
                    "模块 IMS/VoLTE 配置未通过回读",
                )
                .await
                .map_err(|e| e.backup(&backup))?;
            if !result.verified {
                return Err(
                    Failure::new("模块 IMS/VoLTE 配置未通过回读", action_detail(&result))
                        .backup(&backup),
                );
            }
        }
        self.idle(op, lease, "模块配置已保存，重启已暂停", &backup)
            .await?;
        status.state = "restarting".into();
        status.summary = "模块正在重启并重新枚举".into();
        status.voice_configured = true;
        status.detail = if recovery {
            "ADB/UAC 与 IMS/VoLTE 配置已正确，但 ADB 没有响应；正在受控重启并重新验证。"
        } else {
            "网络、短信和模块音频接口会短暂中断，通常需要 20–60 秒恢复。"
        }
        .into();
        self.publish(op, status.clone())?;
        let reboot = self.reboot(op, lease, &backup).await?;
        let mut probe_after = tokio::time::Instant::now() + ADB_SETTLE;
        let deadline = tokio::time::Instant::now() + RECONNECT_BUDGET;
        status.state = "verifying_usb".into();
        status.summary = "正在等待模块重连并验证接口".into();
        status.detail =
            "将核对同一模块、完整 USB 配置、IMS/VoLTE 回读和真实 ADB root 接口。".into();
        self.publish(op, status.clone())?;
        let mut last = reboot;
        let mut authorized_after_reboot = !authorization;
        let mut bound = false;
        while tokio::time::Instant::now() < deadline {
            self.current(op)?;
            let binding = if bound {
                Ok(())
            } else {
                self.0
                    .radio
                    .call_mode_rebind(lease, &identity.imei)
                    .await
                    .map(|_| {
                        bound = true;
                    })
            };
            match binding {
                Err(error) => last = error.public_message().into(),
                Ok(_) => {
                    let result = async {
                        let usb = match self.usb(op, lease).await {
                            Ok(v) => v,
                            Err(e) => {
                                bound = false;
                                return Err(e);
                            }
                        };
                        if usb != target {
                            return Err(Failure::new(
                                "模块已连接，但完整 USB 配置与目标值不一致",
                                "完整 USB 配置与目标值不一致",
                            ));
                        }
                        let voice = match self.voice(op, lease).await {
                            Ok(v) => v,
                            Err(e) => {
                                bound = false;
                                return Err(e);
                            }
                        };
                        if !ready_voice(voice) {
                            return Err(Failure::new(
                                "模块已连接，但 IMS/VoLTE 尚未就绪",
                                format!(
                                    "IMS={}、VoLTE capability={}、volte_disabled={}",
                                    voice.ims, voice.volte_capability, voice.volte_disabled
                                ),
                            ));
                        }
                        Ok(())
                    }
                    .await;
                    match result {
                        Err(error) => last = format!("{}：{}", error.summary, error.detail),
                        Ok(()) if tokio::time::Instant::now() < probe_after => {
                            last = format!(
                                "模块已重连，正在等待 ADB daemon 稳定（约 {} 秒）",
                                (probe_after - tokio::time::Instant::now()).as_secs()
                            )
                        }
                        Ok(()) if !authorized_after_reboot => {
                            status.detail="模块已重连；正在使用新的挑战值重新提交 ADB 授权，然后验证 root 通道。".into();
                            self.publish(op, status.clone())?;
                            let value = self
                                .action(
                                    op,
                                    lease,
                                    Action::CallModeAdbAuthorize,
                                    "模块重启后未接受 ADB 授权",
                                )
                                .await
                                .map_err(|e| e.backup(&backup))?;
                            if !value.verified || !value.submitted {
                                return Err(Failure::new(
                                    "模块重启后未接受 ADB 授权",
                                    action_detail(&value),
                                )
                                .backup(&backup));
                            }
                            authorized_after_reboot = true;
                            probe_after = tokio::time::Instant::now() + Duration::from_secs(2);
                            last = "模块已重新接受 ADB 授权，正在等待 daemon 应用".into();
                        }
                        Ok(()) => match self.probe(op).await {
                            Ok(()) => return self.finish_enable(op, lease, &backup).await,
                            Err(error) => last = error.detail,
                        },
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let runtime = crate::call_mode_runtime::installed_runtime_verification(&self.0.root).await;
        status.state = "needs_interface_recovery".into();
        status.summary = "模块重连后仍未完成通话接口验证".into();
        status.detail = last;
        status.runtime_downloaded = runtime.runtime_downloaded;
        status.downloaded_bytes = runtime.downloaded_bytes;
        status.can_enable = true;
        status.can_retry = true;
        status.requires_usb_confirmation = true;
        self.publish(op, status)
    }

    async fn reboot(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        backup: &str,
    ) -> Result<String, Failure> {
        self.current(op)?;
        let diagnostic = match self.0.radio.call_mode_action(lease, Action::Reboot).await {
            Ok(outcome) if outcome.error_code == "radio_reboot_rejected" => {
                return Err(Failure::new("模块拒绝重启命令", outcome.response).backup(backup));
            }
            Ok(outcome) if outcome.submitted => action_detail(&outcome),
            Ok(outcome) => {
                return Err(Failure::new("模块重启未提交", action_detail(&outcome)).backup(backup));
            }
            Err(error) if error.public_code() == "radio_reboot_rejected" => {
                return Err(Failure::new("模块拒绝重启命令", error.public_message()).backup(backup));
            }
            Err(error) if error.public_submitted() => error.public_message().into(),
            Err(error) => {
                return Err(Failure::new("模块重启未提交", error.public_message()).backup(backup));
            }
        };
        self.current(op)?;
        crate::call_mode_adb::invalidate_native_runtime(&self.0.inbox, &op.producer_id)
            .map_err(|e| Failure::new("模块语音状态失效未确认", e.to_string()).backup(backup))?;
        Ok(diagnostic)
    }

    async fn restore(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        captured: &crate::call_mode_backups::CapturedNativeBackup,
    ) -> Result<VerifiedRestore, Failure> {
        self.verify_restore_input(captured)
            .await
            .map_err(|e| Failure::new("解析还原备份失败", e))?;
        let id = captured.id();
        let backup = captured.backup();
        self.idle(op, lease, "无法还原模块配置", "").await?;
        let identity = self
            .0
            .radio
            .call_mode_identity(lease)
            .await
            .map_err(|e| Failure::new("无法确认当前模块身份", e.public_message()))?;
        let current = self.usb(op, lease).await?;
        let voice = self.voice(op, lease).await?;
        validate_restore_binding(&backup, &identity.imei, current)?;
        // The durable intent precedes even settlement/STOP: accepted native
        // cleanup can itself have real effects before configuration changes.
        let journal = self
            .begin_restore_journal(op, lease, captured, &identity.imei, current, voice)
            .await?;
        let usb_changed = current != backup.usb;
        let voice_changed = backup.voice.is_some_and(|v| !same_voice(v, voice));
        if !usb_changed && !voice_changed {
            // Equal persistent configuration does not settle an accepted
            // native audio obligation. Drain its actual original owner first;
            // a process with no such obligation must not invent an ADB probe.
            crate::call_mode_adb::settle_owned_native_runtime_for_restore(
                &self.0.root,
                self.0.inbox.clone(),
                self.0.usb_at.clone(),
                self.0.radio.clone(),
                op.producer_id.clone(),
                lease,
            )
            .await
            .map_err(|(summary, detail)| Failure::new(summary, detail))?;
            self.idle(op, lease, "还原确认已停止", "").await?;
            let settled_identity = self
                .0
                .radio
                .call_mode_identity(lease)
                .await
                .map_err(|e| Failure::new("无法确认清理后的模块身份", e.public_message()))?;
            let settled_usb = self.usb(op, lease).await?;
            let settled_voice = self.voice(op, lease).await?;
            validate_restore_binding(&backup, &settled_identity.imei, settled_usb)?;
            if settled_usb != backup.usb
                || backup
                    .voice
                    .is_some_and(|target| !same_voice(target, settled_voice))
            {
                return Err(Failure::new(
                    "清理后的模块配置尚未确认还原",
                    "实际音频清理期间配置发生变化，未提交成功回执或新的配置写入",
                ));
            }
            return self.finish_restore(op, lease, captured, "", false).await;
        }
        crate::call_mode_adb::settle_owned_native_runtime_for_restore(
            &self.0.root,
            self.0.inbox.clone(),
            self.0.usb_at.clone(),
            self.0.radio.clone(),
            op.producer_id.clone(),
            lease,
        )
        .await
        .map_err(|(s, d)| Failure::new(s, d))?;
        self.current(op)?;
        let proof = self
            .0
            .radio
            .module_recovery()
            .root_proof_for(&self.0.root)
            .map_err(|()| Failure::new("无法保存还原前保护备份", "数据目录锁归属未确认"))?;
        let safety_id = crate::call_mode_backups::create_native_backup(
            &self.0.root,
            identity.clone(),
            current,
            voice,
            true,
            &proof,
        )
        .await
        .map_err(|e| Failure::new("无法保存还原前保护备份", e))?;
        let safety_capture =
            crate::call_mode_backups::capture_native_backup(&self.0.root, &safety_id, &proof)
                .await
                .map_err(|e| Failure::new("无法确认还原前保护备份", e))?;
        journal
            .safety_checkpoint(&safety_id, &safety_capture.sha256())
            .map_err(|e| Failure::new("无法持久记录还原前保护备份", e.to_string()))?;
        let safety = self.backup_path(&safety_id);
        self.idle(op, lease, "还原已安全停止", &safety).await?;
        let mut status = Status::new("restoring_usb", "正在还原模块 USB 配置");
        status.adb_enabled = current.flags[5] == 1;
        status.uac_enabled = current.flags[6] == 1;
        status.voice_configured = ready_voice(voice);
        status.interfaces_ready =
            status.adb_enabled && status.uac_enabled && status.voice_configured;
        status.runtime_downloaded =
            crate::call_mode_runtime::installed_runtime_verified(&self.0.root).await;
        status.backup_path = self.backup_path(id);
        status.detail =
            "已自动保存当前 USB 与 IMS/VoLTE 配置，正在写入所选备份并进行完整回读。".into();
        self.publish(op, status.clone())?;
        if usb_changed {
            let value = self
                .action(
                    op,
                    lease,
                    Action::CallModeUsbCompositionWrite {
                        expected: current,
                        target: backup.usb,
                    },
                    "还原 USB 配置写入或回读未通过，模块尚未重启",
                )
                .await
                .map_err(|e| e.backup(&safety))?;
            if !value.verified {
                return Err(Failure::new(
                    "还原 USB 配置写入或回读未通过，模块尚未重启",
                    action_detail(&value),
                )
                .backup(&safety));
            }
        }
        if voice_changed {
            let value = self
                .action(
                    op,
                    lease,
                    Action::CallModeVoice {
                        target: backup.voice.expect("changed voice has target"),
                    },
                    "还原 IMS/VoLTE 配置回读失败，模块尚未重启",
                )
                .await
                .map_err(|e| e.backup(&safety))?;
            if !value.verified {
                return Err(Failure::new(
                    "还原 IMS/VoLTE 配置回读失败，模块尚未重启",
                    action_detail(&value),
                )
                .backup(&safety));
            }
        }
        status.state = "restarting_restore".into();
        status.summary = "模块正在重启并应用还原配置".into();
        status.adb_enabled = backup.usb.flags[5] == 1;
        status.uac_enabled = backup.usb.flags[6] == 1;
        status.voice_configured = backup
            .voice
            .map(ready_voice)
            .unwrap_or_else(|| ready_voice(voice));
        status.detail =
            "网络、短信、ADB、IMS/VoLTE 和模块音频接口会按备份重新枚举，通常需要 20–60 秒。".into();
        self.publish(op, status)?;
        let mut last = self.reboot(op, lease, &safety).await?;
        let deadline = tokio::time::Instant::now() + RECONNECT_BUDGET;
        let mut bound = false;
        while tokio::time::Instant::now() < deadline {
            self.current(op)?;
            let outcome = async {
                if !bound {
                    self.0
                        .radio
                        .call_mode_rebind(lease, &backup.module.imei)
                        .await
                        .map_err(|e| Failure::new("原模块重连尚未确认", e.public_message()))?;
                    bound = true;
                }
                let usb = match self.usb(op, lease).await {
                    Ok(v) => v,
                    Err(e) => {
                        bound = false;
                        return Err(e);
                    }
                };
                if usb != backup.usb {
                    return Err(Failure::new(
                        "模块已连接，但 USB 配置尚未恢复为备份值",
                        "完整 USB 配置回读不一致",
                    ));
                }
                if let Some(target) = backup.voice {
                    let voice = match self.voice(op, lease).await {
                        Ok(v) => v,
                        Err(e) => {
                            bound = false;
                            return Err(e);
                        }
                    };
                    if !same_voice(voice, target) {
                        return Err(Failure::new(
                            "模块已连接，但 IMS/VoLTE 配置尚未恢复为备份值",
                            "可写 IMS/VoLTE 值回读不一致",
                        ));
                    }
                }
                Ok(())
            }
            .await;
            match outcome {
                Ok(()) => {
                    return self
                        .finish_restore(op, lease, captured, &safety_id, true)
                        .await;
                }
                Err(e) => last = format!("{}：{}", e.summary, e.detail),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(Failure::new("等待还原后的模块重新连接超时", last).backup(&safety))
    }
    async fn finish_restore(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        captured: &crate::call_mode_backups::CapturedNativeBackup,
        safety: &str,
        changed: bool,
    ) -> Result<VerifiedRestore, Failure> {
        self.current(op)?;
        self.0
            .radio
            .validate_call_mode_operation_lock(lease)
            .map_err(|e| Failure::new("还原操作锁归属未确认", e.public_message()))?;
        self.verify_restore_input(captured)
            .await
            .map_err(|e| Failure::new("还原输入归属确认失败", e))?;
        let id = captured.id();
        let backup = captured.backup();
        let mut status = crate::call_mode_status::inspect_with_reservation(
            &self.0.root,
            &self.0.radio,
            lease,
            &op.producer_id,
            self.0.inbox.clone(),
        )
        .await;
        // Readiness projection can return a diagnostic Status after failed
        // reads. Only a fresh, typed target verification authorizes a restore
        // receipt; the original backup may legitimately disable ADB/UAC.
        self.verify_restored_target(op, lease, backup).await?;
        status.backup_path = self.backup_path(id);
        status.last_restore = Some(
            json!({"backup_id":id,"backup_saved_at":backup.saved_at,"backup_sha256":captured.sha256(),"restored_at":timestamp(),"safety_backup_id":if safety.is_empty(){"."}else{safety},"changed":changed}),
        );
        let restored_detail = if !changed {
            "当前模块配置已经与所选备份一致，没有写入或重启模块。"
        } else if backup.voice.is_some() {
            "原始 USB 与 IMS/VoLTE 配置已还原并回读确认；还原前的配置也已自动保存为保护备份。"
        } else {
            "旧版备份中的 USB 配置已还原并回读确认；该备份不包含 IMS/VoLTE，相关配置未改变。"
        };
        status.detail = if status.detail.is_empty() {
            restored_detail.into()
        } else {
            format!("{}\n{restored_detail}", status.detail)
        };
        self.verify_restore_input(captured)
            .await
            .map_err(|e| Failure::new("还原回执的输入归属未确认", e))?;
        let namespace = self
            .0
            .radio
            .capture_call_mode_operation_namespace(lease)
            .map_err(|e| Failure::new("还原回执的操作锁归属未确认", e.public_message()))?;
        let journal = self.restore_journal(op)?;
        let durable_receipt = RestoreReceipt {
            // Completion time is only filled after actual release ACK.
            restored_at: String::new(),
            changed,
            module_imei: backup.module.imei.clone(),
            usb: backup.usb,
            voice: backup.voice,
        };
        journal
            .target_verified(durable_receipt.clone())
            .map_err(|e| Failure::new("无法持久记录还原目标回读", e.to_string()))?;
        // Do not expose last_restore while the original reservation or its
        // accepted borrowed workers still own physical cleanup obligations.
        Ok(VerifiedRestore {
            status,
            namespace,
            journal,
            durable_receipt,
        })
    }

    async fn verify_restored_target(
        &self,
        op: &Operation,
        lease: &CallModeReservation,
        backup: &crate::call_mode_backups::NativeBackup,
    ) -> Result<(), Failure> {
        self.current(op)?;
        self.0
            .radio
            .validate_call_mode_operation_lock(lease)
            .map_err(|e| Failure::new("还原回读的操作锁归属未确认", e.public_message()))?;
        let identity = self
            .0
            .radio
            .call_mode_identity(lease)
            .await
            .map_err(|e| Failure::new("无法确认还原回执所属模块", e.public_message()))?;
        self.current(op)?;
        let usb = self.usb(op, lease).await?;
        let voice = self.voice(op, lease).await?;
        validate_restore_binding(backup, &identity.imei, usb)?;
        if usb != backup.usb
            || backup
                .voice
                .is_some_and(|target| !same_voice(target, voice))
        {
            return Err(Failure::new(
                "还原回执的模块配置尚未确认",
                "最终完整 USB 或 IMS/VoLTE 回读与所选备份不一致，未发布还原成功回执",
            ));
        }
        Ok(())
    }

    fn producer_current(&self, producer: &str) -> bool {
        self.0.inbox.owns_module_producer(producer)
            && self
                .0
                .cancelled_producer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_deref()
                != Some(producer)
    }
    fn observation_producer(&self) -> Option<String> {
        let producer = self.0.inbox.current_module_producer_id()?;
        if !self.producer_current(&producer) {
            return None;
        }
        let mut previous = self
            .0
            .status_producer
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *previous != producer && !self.0.state.running() {
            let (revision, _) = self.0.state.observation();
            self.0
                .state
                .publish_observation(revision, Status::new("checking", "正在检查通话模式"));
            *previous = producer.clone();
        }
        Some(producer)
    }
    fn pending_adb_status(&self) -> Option<Status> {
        let producer = self.0.inbox.current_module_producer_id()?;
        if !self.producer_current(&producer) {
            return None;
        }
        let pending = crate::call_mode_adb::pending_probe(&self.0.root, &producer)?;
        let mut status = self.0.state.snapshot();
        if !self.0.state.running() {
            status.state = "checking".into();
            status.summary = "正在等待隔离 ADB 进程清理".into();
        }
        status.detail = pending.message;
        status.can_enable = false;
        status.can_download = false;
        status.can_retry = false;
        Some(status)
    }
    pub(crate) async fn public_status(&self) -> Status {
        self.observe_refresh(false).await
    }
    fn conservative_observation(&self) -> Status {
        if let Some(status) = self.pending_adb_status() {
            return status;
        }
        let mut status = self.0.state.snapshot();
        if !self.0.state.running() {
            status.state = "checking".into();
            status.summary = "正在检查通话模式".into();
        }
        status.can_enable = false;
        status.can_download = false;
        status.can_retry = false;
        status
    }
    async fn observe_refresh(&self, force: bool) -> Status {
        if let Some(status) = self.pending_adb_status() {
            return status;
        }
        // Check retained ownership before looking up a replacement identity.
        // No-current/revoked producers cannot recover an old snapshot's controls.
        if self
            .0
            .active_observation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return self.conservative_observation();
        }
        let Some(producer) = self.0.inbox.current_module_producer_id() else {
            return self.conservative_observation();
        };
        if !self.producer_current(&producer) {
            return self.conservative_observation();
        }
        let token = Arc::new(());
        {
            // Register before spawn. Repeated observers cannot queue a refresh
            // which starts new USB work after this original probe settles.
            let mut active = self
                .0
                .active_observation
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = active.as_ref() {
                // An outstanding identity cannot transfer controls to a
                // replacement producer. Keep the original owner's token and
                // project no new admission rights for either identity.
                let same_identity = entry.root == self.0.root && entry.producer == producer;
                drop(active);
                let mut status = self.conservative_observation();
                if !same_identity && !self.0.state.running() {
                    status.summary = "正在等待原模块连接清理".into();
                }
                return status;
            }
            *active = Some(ActiveObservation {
                root: self.0.root.clone(),
                producer: producer.clone(),
                token: token.clone(),
            });
        }
        let observation = ObservationOwner {
            inner: self.0.clone(),
            token,
        };
        let pin = ReadPin::new(self.0.clone());
        let owner = self.clone();
        let mut refresh = tokio::spawn(async move {
            let result = owner.refresh(force).await;
            drop(pin);
            drop(observation);
            result
        });
        loop {
            tokio::select! {
                result = &mut refresh => {
                    let Ok(status) = result else { return self.conservative_observation(); };
                    let successor = self.0.active_observation.lock().unwrap_or_else(|e| e.into_inner()).is_some();
                    if !self.producer_current(&producer) || successor {
                        return self.conservative_observation();
                    }
                    return status;
                },
                _ = tokio::time::sleep(Duration::from_millis(25)) => {
                    // Drop only this observer handle. The accepted refresh owns
                    // its ReadPin/reservation through exact child settlement.
                    if let Some(status) = self.pending_adb_status() { return status; }
                }
            }
        }
    }
    async fn refresh(&self, force: bool) -> Status {
        if let Some(status) = self.pending_adb_status() {
            return status;
        }
        if self.0.radio.module_recovery().is_quarantined() {
            return crate::module_recovery::call_mode_status(&self.0.root);
        }
        let Ok(_refresh) = self.0.refresh.try_lock() else {
            return self.0.state.snapshot();
        };
        let Some(producer) = self.observation_producer() else {
            return self.0.state.snapshot();
        };
        let (revision, status) = self.0.state.observation();
        if self.0.stopping.load(Ordering::Acquire)
            || self.0.state.running()
            || self.0.radio.vowifi_owns_sim_channel()
            || (!force && (status.state == "failed" || status.state.starts_with("needs_")))
        {
            return status;
        }
        if !force
            && status.state == "ready"
            && crate::call_mode_adb::is_module_voice_runtime_prepared(
                self.0.radio.module_recovery(),
                &self.0.root,
                &producer,
                self.0.inbox.clone(),
            )
            .await
        {
            return self.0.state.snapshot();
        }
        let lease = match self.0.radio.reserve_call_mode_read().await {
            Ok(lease) => lease,
            Err(error) => {
                if error.public_status() == StatusCode::CONFLICT {
                    return self.0.state.snapshot();
                }
                let mut disconnected = Status::new("disconnected", "等待 4G 模块连接");
                disconnected.detail = error.public_message().into();
                disconnected.can_retry = true;
                if self.producer_current(&producer) {
                    self.0.state.publish_observation(revision, disconnected);
                }
                return self.0.state.snapshot();
            }
        };
        // Own the complete read transaction, including ADB child workers, even
        // when the HTTP observer disconnects. Retirement drains this ReadPin.
        let observed = self.inspect_read(&lease, &producer).await;
        let prepared = if observed.state == "ready" {
            crate::call_mode_adb::is_module_voice_runtime_prepared(
                self.0.radio.module_recovery(),
                &self.0.root,
                &producer,
                self.0.inbox.clone(),
            )
            .await
        } else {
            false
        };
        self.release_until_confirmed(&lease, None).await;
        if !self.producer_current(&producer) {
            return self.0.state.snapshot();
        }
        let prepare = observed.state == "ready" && !prepared;
        if !self.0.state.publish_observation(revision, observed.clone()) {
            return self.0.state.snapshot();
        }
        if prepare && !self.0.stopping.load(Ordering::Acquire) {
            let backup = observed.backup_path.clone();
            let source = observed.source.clone();
            let status = ready_status(
                "preparing",
                "正在部署并验证模块侧语音运行时",
                &backup,
                &source,
            );
            let _ = self.start(Job::Prepare(backup, source), status).await;
        }
        self.0.state.snapshot()
    }
    async fn inspect_read(&self, lease: &CallModeReservation, producer: &str) -> Status {
        #[cfg(test)]
        {
            let probe = self
                .0
                .refresh_probe
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(probe) = probe {
                let result =
                    crate::call_mode_adb::child_tests::run_probe_for(probe, producer).await;
                let mut status = Status::new("failed", "controlled child result");
                status.detail = result
                    .err()
                    .unwrap_or_else(|| "controlled probe exited".into());
                status.can_retry = true;
                return status;
            }
        }
        crate::call_mode_status::inspect_with_reservation(
            &self.0.root,
            &self.0.radio,
            lease,
            producer,
            self.0.inbox.clone(),
        )
        .await
    }
    #[cfg(test)]
    pub(crate) fn install_refresh_probe_for_tests(
        &self,
        probe: crate::call_mode_adb::child_tests::HeldProbe,
    ) {
        *self
            .0
            .refresh_probe
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(probe);
    }
    #[cfg(test)]
    pub(crate) fn active_status_reads_for_tests(&self) -> usize {
        self.0.active_reads.load(Ordering::Acquire)
    }
    pub(crate) async fn retry(&self) -> Reply {
        if let Some(status) = self.pending_adb_status() {
            return crate::json_response(StatusCode::CONFLICT, &status);
        }
        if self
            .0
            .active_observation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return crate::json_response(StatusCode::CONFLICT, &self.conservative_observation());
        }
        if !self
            .0
            .inbox
            .current_module_producer_id()
            .is_some_and(|producer| self.producer_current(&producer))
        {
            return crate::json_response(StatusCode::OK, &self.conservative_observation());
        }
        let cached = self.0.state.snapshot();
        if self.0.state.running() {
            return failure(StatusCode::CONFLICT, "通话模式正在执行其他操作");
        }
        if !cached.can_retry && cached.state != "checking" && cached.state != "disconnected" {
            return crate::json_response(StatusCode::OK, &cached);
        }
        let status = self.observe_refresh(true).await;
        crate::json_response(
            if self.0.state.running() {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            },
            &status,
        )
    }
    pub(crate) async fn ensure_ready(&self, producer: &str) -> Result<(), String> {
        if self.0.radio.module_recovery().is_admission_revoked() {
            return Err(crate::module_recovery::CODE.into());
        }
        let _pin = ReadPin::new(self.0.clone());
        if !self.producer_current(producer) {
            return Err("后端代次已变化，请重新确认通话模式就绪状态".into());
        }
        if self.0.stopping.load(Ordering::Acquire) || self.0.state.running() {
            return Err("模块配置或通话模式正在变更，请等待操作完成".into());
        }
        let lock = crate::call_mode_backups::try_call_mode_operation_shared_lock(&self.0.root)
            .map_err(|error| format!("无法确认模块通话模式操作状态：{error}"))?;
        crate::call_audio::call_readiness(
            &self.0.root,
            &self.0.inbox,
            self.0.radio.clone(),
            producer.to_owned(),
        )
        .await?;
        drop(lock);
        if !self.producer_current(producer) {
            return Err("后端代次已变化，请重新确认通话模式就绪状态".into());
        }
        Ok(())
    }
    async fn readiness(&self, producer: String) -> Reply {
        if self.0.radio.module_recovery().is_admission_revoked() {
            return crate::module_recovery::rejection();
        }
        let _pin = ReadPin::new(self.0.clone());
        if !self.producer_current(&producer) {
            return failure(
                StatusCode::CONFLICT,
                "后端代次已变化，请重新确认通话模式就绪状态",
            );
        }
        if self.0.stopping.load(Ordering::Acquire) || self.0.state.running() {
            return crate::json_response(
                StatusCode::OK,
                &json!({"ready":false,"operation_running":true,"error":"模块配置或通话模式正在变更，请等待操作完成"}),
            );
        }
        let lock = match crate::call_mode_backups::try_call_mode_operation_shared_lock(&self.0.root)
        {
            Ok(lock) => lock,
            Err(error) => {
                return crate::json_response(
                    StatusCode::OK,
                    &json!({"ready":false,"operation_running":self.0.state.running(),"error":format!("无法确认模块通话模式操作状态：{error}")}),
                );
            }
        };
        let result = crate::call_audio::call_readiness(
            &self.0.root,
            &self.0.inbox,
            self.0.radio.clone(),
            producer.clone(),
        )
        .await;
        drop(lock);
        if !self.producer_current(&producer) {
            return failure(
                StatusCode::CONFLICT,
                "后端代次已变化，请重新确认通话模式就绪状态",
            );
        }
        match result {
            Ok(_) => crate::json_response(
                StatusCode::OK,
                &json!({"ready":true,"operation_running":false}),
            ),
            Err(error) => crate::json_response(
                StatusCode::OK,
                &json!({"ready":false,"operation_running":false,"error":error}),
            ),
        }
    }
    pub(crate) fn begin_shutdown(&self) {
        self.0.stopping.store(true, Ordering::Release);
    }
    async fn wait_operations(&self) {
        loop {
            let finished = self.0.finished.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            if !self.0.state.running() && self.0.active_reads.load(Ordering::Acquire) == 0 {
                return;
            }
            finished.await;
        }
    }
    pub(crate) async fn retire_producer(&self, producer: &str) -> Result<(), String> {
        if !self.0.inbox.owns_module_producer(producer) {
            return Ok(());
        }
        *self
            .0
            .cancelled_producer
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(producer.into());
        tokio::time::timeout(Duration::from_secs(180), async {
            self.wait_operations().await;
            // A captured-producer shutdown owns this gate through real ADB
            // cleanup; replacement must not overtake it.
            let _gate = self.0.shutdown_gate.lock().await;
            self.wait_operations().await;
        })
        .await
        .map_err(|_| "通话模式旧代操作尚未实际退出".to_owned())
    }
    async fn shutdown_producer(&self, producer: String) -> Result<(), String> {
        let _gate = self.0.shutdown_gate.lock().await;
        if !self.producer_current(&producer) {
            return Err("通话模式退出请求所属后端已过期".into());
        }
        *self
            .0
            .cancelled_producer
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(producer.clone());
        self.wait_operations().await;
        if !self.0.inbox.owns_module_producer(&producer) {
            return Err("通话模式退出请求所属后端已过期".into());
        }
        if !self.requires_runtime_cleanup().await {
            return Ok(());
        }
        crate::call_mode_adb::stop_native_runtime(
            &self.0.root,
            self.0.inbox.clone(),
            self.0.usb_at.clone(),
            self.0.radio.clone(),
            producer,
            None,
        )
        .await
        .map_err(|(_, detail)| detail)
    }
    pub(crate) async fn finish_shutdown(&self) -> Result<(), String> {
        self.begin_shutdown();
        let _gate = self.0.shutdown_gate.lock().await;
        self.wait_operations().await;
        let Some(producer) = self.0.inbox.current_module_producer_id() else {
            return Ok(());
        };
        if !self.requires_runtime_cleanup().await {
            return Ok(());
        }
        crate::call_mode_adb::stop_native_runtime(
            &self.0.root,
            self.0.inbox.clone(),
            self.0.usb_at.clone(),
            self.0.radio.clone(),
            producer,
            None,
        )
        .await
        .map_err(|(_, detail)| detail)
    }

    /// Final-only receipt. Cache the attempt under the shutdown gate: polling
    /// an unknown result must not blindly replay an ADB mutation.
    pub(crate) async fn finish_shutdown_receipt(&self) -> Result<Vec<String>, String> {
        self.begin_shutdown();
        let this = self.clone();
        tokio::spawn(async move {
            let _gate = this.0.shutdown_gate.lock().await;
            {
                let mut receipt = this
                    .0
                    .shutdown_receipt
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(result) = &*receipt {
                    return result.clone();
                }
                *receipt = Some(Err("call mode finalizer unconfirmed".into()));
            }
            this.wait_operations().await;
            let result = if !this.requires_runtime_cleanup().await {
                Ok(Vec::new())
            } else if let Some(producer) = this.0.inbox.current_module_producer_id() {
                crate::call_mode_adb::stop_native_runtime_shutdown_receipt(
                    &this.0.root,
                    this.0.inbox.clone(),
                    this.0.usb_at.clone(),
                    this.0.radio.clone(),
                    producer,
                )
                .await
            } else {
                Err("call mode cleanup has no current module producer".into())
            };
            *this
                .0
                .shutdown_receipt
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
            result
        })
        .await
        .map_err(|e| format!("call mode finalizer failed: {e}"))?
    }

    /// A producer exists even on a daemon that has never found a modem. Only
    /// affirmative pristine state permits omitting ADB shutdown. Cached
    /// "disconnected" alone is not proof: prior operations, direct audio-route
    /// workers and previous processes can still owe physical cleanup.
    async fn requires_runtime_cleanup(&self) -> bool {
        if self.0.operation_started.load(Ordering::Acquire) || self.0.state.running() {
            return true;
        }
        let status = self.0.state.snapshot();
        if status.adb_enabled
            || status.interfaces_ready
            || status.adb_authorization_accepted
            || status.uac_enabled
            || status.voice_configured
            || !status.backup_path.is_empty()
            || status.last_restore.is_some()
        {
            return true;
        }
        // Both lock files can be created by read-only status probes. A valid
        // pristine marker proves this daemon began with no legacy evidence and
        // no physical start has been admitted since. Old roots without it
        // remain conservative, including an unknown crash between mutation
        // and status publication.
        let root = self.0.root.clone();
        let recovery = self.0.radio.module_recovery().clone();
        crate::archive_io::run(move || {
            if recovery.has_clean_legacy_lock_provenance(&root) {
                return Ok(false);
            }
            let pristine = match pristine_marker(&root) {
                Ok(pristine) => pristine,
                Err(_) => return Ok(true),
            };
            if !pristine && (status.state != "disconnected" || status.runtime_downloaded) {
                return Ok(true);
            }
            match fs::symlink_metadata(root.join("module-backups")) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                // A complete fresh data-only lifecycle can account for this
                // one slot. The real Gate also rechecks all other evidence;
                // no physical-operation or cached-status guard is bypassed.
                Ok(_) if pristine && recovery.has_clean_fresh_backup_data(&root) => {}
                Ok(_) | Err(_) => return Ok(true),
            }
            if pristine {
                return Ok(false);
            }
            for path in [
                "call-mode-operation.lock",
                ".module-adb-interface.lock",
                "voice-runtime",
            ] {
                match fs::symlink_metadata(root.join(path)) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Ok(_) | Err(_) => return Ok(true),
                }
            }
            Ok(false)
        })
        .await
        .unwrap_or(true)
    }

    pub(crate) async fn handle_internal(&self, request: Request<Incoming>) -> Reply {
        let path = request.uri().path().to_owned();
        if !self.0.inbox.compatibility_module_allowed() {
            return failure(StatusCode::CONFLICT, "模块生命周期由 Rust 原生归属管理");
        }
        if request.uri().query().is_some() {
            return failure(StatusCode::NOT_FOUND, "route not found");
        }
        if path == format!("{PREFIX}status") && request.method() == Method::GET {
            return crate::json_response(StatusCode::OK, &self.public_status().await);
        }
        if request.method() != Method::POST
            || !matches!(
                path.strip_prefix(PREFIX),
                Some("readiness" | "invalidate" | "shutdown")
            )
        {
            return failure(StatusCode::NOT_FOUND, "route not found");
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            producer_id: String,
        }
        let body = match crate::http_server::collect_body_with_timeout(
            std::pin::pin!(request.into_body()),
            1024,
            Duration::from_secs(3),
        )
        .await
        {
            Ok(body) => body,
            _ => return failure(StatusCode::BAD_REQUEST, "无效通话模式请求"),
        };
        let input: Input = match serde_json::from_slice(&body) {
            Ok(input) => input,
            Err(_) => return failure(StatusCode::BAD_REQUEST, "无效通话模式请求"),
        };
        if !self.0.inbox.owns_current_producer(&input.producer_id)
            || !self.0.inbox.owns_module_producer(&input.producer_id)
        {
            return failure(StatusCode::CONFLICT, "后端代次已变化，请刷新后重试");
        }
        match path.strip_prefix(PREFIX) {
            Some("readiness") => {
                let owner = self.clone();
                tokio::spawn(async move { owner.readiness(input.producer_id).await })
                    .await
                    .unwrap_or_else(|_| {
                        failure(StatusCode::SERVICE_UNAVAILABLE, "通话模式就绪检查未完成")
                    })
            }
            Some("invalidate") => match crate::call_mode_adb::invalidate_native_runtime(
                &self.0.inbox,
                &input.producer_id,
            ) {
                Ok(_) => crate::json_response(StatusCode::OK, &json!({"ready":true})),
                Err(error) => failure(StatusCode::CONFLICT, error.to_string()),
            },
            Some("shutdown") => {
                let owner = self.clone();
                match tokio::spawn(async move { owner.shutdown_producer(input.producer_id).await })
                    .await
                {
                    Ok(Ok(())) => crate::json_response(StatusCode::OK, &json!({"ready":true})),
                    Ok(Err(error)) => failure(StatusCode::SERVICE_UNAVAILABLE, error),
                    Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "模块语音退出未确认"),
                }
            }
            _ => failure(StatusCode::NOT_FOUND, "route not found"),
        }
    }
}

struct ReadPin(Arc<Inner>);
impl ReadPin {
    fn new(inner: Arc<Inner>) -> Self {
        inner.active_reads.fetch_add(1, Ordering::AcqRel);
        Self(inner)
    }
}
impl Drop for ReadPin {
    fn drop(&mut self) {
        self.0.active_reads.fetch_sub(1, Ordering::AcqRel);
        self.0.finished.notify_waiters();
    }
}

fn validate_restore_binding(
    backup: &crate::call_mode_backups::NativeBackup,
    imei: &str,
    usb: UsbComposition,
) -> Result<(), Failure> {
    if imei.len() != 15 || !imei.bytes().all(|b| b.is_ascii_digit()) || backup.module.imei != imei {
        return Err(Failure::new(
            "备份与当前模块不匹配",
            "备份所属模块与当前连接模块的 IMEI 不一致",
        ));
    }
    if backup.usb.vendor_id != usb.vendor_id || backup.usb.product_id != usb.product_id {
        return Err(Failure::new(
            "备份与当前模块不匹配",
            "备份与当前模块的 USB VID/PID 不一致",
        ));
    }
    Ok(())
}

fn ready_status(state: &str, summary: &str, backup: &str, source: &str) -> Status {
    let mut status = Status::new(state, summary);
    status.adb_enabled = true;
    status.uac_enabled = true;
    status.interfaces_ready = true;
    status.voice_configured = true;
    status.runtime_downloaded = true;
    status.downloaded_bytes = crate::call_mode_runtime_manifest::manifest()
        .files
        .iter()
        .map(|file| file.size)
        .sum();
    status.backup_path = backup.into();
    status.source = source.into();
    status
}
fn download_status(source: &str) -> Status {
    let mut status = ready_status("downloading", "正在下载模块侧语音运行时", "", source);
    status.runtime_downloaded = false;
    status.downloaded_bytes = 0;
    status
}
fn action_detail(outcome: &Outcome) -> String {
    let detail = if outcome.response.trim().is_empty() {
        "模块操作结果未确认"
    } else {
        outcome.response.trim()
    };
    if outcome.error_code.is_empty() {
        detail.into()
    } else {
        format!("{}：{detail}", outcome.error_code)
    }
}

#[cfg(test)]
mod journal_process_tests;
#[cfg(test)]
mod tests;
