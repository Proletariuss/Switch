//! Authoritative native call-mode status, with operation and observation fences.
//!
//! A coordinator keeps its physical reservation until actual worker cleanup;
//! this store never turns a cancelled waiter into a completed operation.
use crate::call_mode_runtime_manifest::manifest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct DownloadSource {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) detail: String,
    pub(crate) trusted: bool,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Status {
    pub(crate) state: String,
    pub(crate) summary: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub(crate) detail: String,
    pub(crate) adb_enabled: bool,
    pub(crate) interfaces_ready: bool,
    pub(crate) adb_authorization_required: bool,
    pub(crate) adb_authorization_accepted: bool,
    pub(crate) uac_enabled: bool,
    pub(crate) voice_configured: bool,
    pub(crate) runtime_downloaded: bool,
    pub(crate) runtime_version: String,
    pub(crate) runtime_path: String,
    pub(crate) downloaded_bytes: u64,
    pub(crate) total_bytes: u64,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub(crate) source: String,
    pub(crate) can_enable: bool,
    pub(crate) can_download: bool,
    pub(crate) can_retry: bool,
    pub(crate) requires_adb_authorization_confirmation: bool,
    pub(crate) requires_usb_confirmation: bool,
    pub(crate) requires_download_confirmation: bool,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub(crate) backup_path: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) last_restore: Option<Value>,
    pub(crate) updated_at: String,
    pub(crate) sources: Vec<DownloadSource>,
    pub(crate) upstream: String,
}
impl Status {
    pub(crate) fn new(state: &str, summary: &str) -> Self {
        Self {
            state: state.into(),
            summary: summary.into(),
            ..Self::default()
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Operation {
    pub(crate) producer_id: String,
    pub(crate) generation: u64,
}
#[derive(Clone)]
pub(crate) struct State(Arc<Inner>);
struct Inner {
    root: PathBuf,
    value: Mutex<Stored>,
}
struct Stored {
    status: Status,
    revision: u64,
    generation: u64,
    operation: Option<Operation>,
}
impl State {
    pub(crate) fn new(data_root: &Path) -> Self {
        let state = Self(Arc::new(Inner {
            root: data_root.into(),
            value: Mutex::new(Stored {
                status: Status::new("disconnected", "等待 4G 模块连接"),
                revision: 1,
                generation: 0,
                operation: None,
            }),
        }));
        {
            let mut value = state.0.value.lock().unwrap_or_else(|e| e.into_inner());
            state.normalize(&mut value.status);
        }
        state
    }
    fn normalize(&self, status: &mut Status) {
        let runtime = manifest();
        status.runtime_version = runtime.version.clone();
        status.runtime_path = self
            .0
            .root
            .join("voice-runtime")
            .join(&runtime.version)
            .to_string_lossy()
            .into_owned();
        status.total_bytes = runtime.files.iter().map(|file| file.size).sum();
        status.downloaded_bytes = status.downloaded_bytes.min(status.total_bytes);
        status.updated_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_default();
        status.upstream = format!(
            "https://github.com/moluncn/mavo/tree/{}/Resources/ModuleVoice",
            runtime.commit
        );
        status.sources = vec![
            DownloadSource {
                id: "github".into(),
                name: "GitHub 原始源".into(),
                detail: "直接从 raw.githubusercontent.com 的固定提交下载".into(),
                trusted: true,
            },
            DownloadSource {
                id: "relay".into(),
                name: "国内转发源".into(),
                detail: "依次尝试 ghfast.top 与 gh-proxy.com，仍使用同一 SHA-256 校验".into(),
                trusted: false,
            },
        ];
    }
    fn replace(&self, stored: &mut Stored, mut status: Status) {
        if status.last_restore.is_none() {
            status.last_restore = stored.status.last_restore.clone();
        }
        self.normalize(&mut status);
        stored.status = status;
        stored.revision = stored.revision.saturating_add(1);
    }
    pub(crate) fn snapshot(&self) -> Status {
        self.observation().1
    }
    pub(crate) fn operation_generation(&self) -> u64 {
        self.0
            .value
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .generation
    }
    pub(crate) fn observation(&self) -> (u64, Status) {
        let value = self.0.value.lock().unwrap_or_else(|e| e.into_inner());
        (value.revision, value.status.clone())
    }
    pub(crate) fn running(&self) -> bool {
        self.0
            .value
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .operation
            .is_some()
    }
    pub(crate) fn publish_observation(&self, revision: u64, status: Status) -> bool {
        let mut value = self.0.value.lock().unwrap_or_else(|e| e.into_inner());
        if value.revision != revision || value.operation.is_some() {
            return false;
        }
        self.replace(&mut value, status);
        true
    }
    pub(crate) fn begin(&self, producer_id: &str, status: Status) -> Option<Operation> {
        let mut value = self.0.value.lock().unwrap_or_else(|e| e.into_inner());
        if value.operation.is_some() || producer_id.is_empty() {
            return None;
        }
        value.generation = value.generation.checked_add(1)?;
        let operation = Operation {
            producer_id: producer_id.into(),
            generation: value.generation,
        };
        value.operation = Some(operation.clone());
        self.replace(&mut value, status);
        Some(operation)
    }
    pub(crate) fn is_current(&self, operation: &Operation) -> bool {
        self.0
            .value
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .operation
            .as_ref()
            == Some(operation)
    }
    pub(crate) fn publish(&self, operation: &Operation, status: Status) -> bool {
        let mut value = self.0.value.lock().unwrap_or_else(|e| e.into_inner());
        if value.operation.as_ref() != Some(operation) {
            return false;
        }
        self.replace(&mut value, status);
        true
    }
    pub(crate) fn end(&self, operation: &Operation) -> bool {
        let mut value = self.0.value.lock().unwrap_or_else(|e| e.into_inner());
        if value.operation.as_ref() != Some(operation) {
            return false;
        }
        value.operation = None;
        value.revision = value.revision.saturating_add(1);
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_observations_and_operations_cannot_replace_current_work() {
        let state = State::new(Path::new("/fixture"));
        let (revision, _) = state.observation();
        let first = state
            .begin("producer-a", Status::new("downloading", "download"))
            .unwrap();
        assert!(!state.publish_observation(revision, Status::new("ready", "old read")));
        assert!(
            state
                .begin("producer-b", Status::new("preparing", "busy"))
                .is_none()
        );
        assert!(state.end(&first));
        let second = state
            .begin("producer-b", Status::new("preparing", "new work"))
            .unwrap();
        assert!(second.generation > first.generation);
        assert!(!state.publish(&first, Status::new("ready", "old worker")));
        assert!(!state.end(&first));
        assert!(state.is_current(&second));
        assert_eq!(state.snapshot().state, "preparing");
    }
    #[test]
    fn fresh_observation_retains_restore_notice_and_complete_public_dto() {
        let state = State::new(Path::new("/fixture"));
        let operation = state
            .begin("p", Status::new("restoring", "restore"))
            .unwrap();
        let mut restored = Status::new("needs_usb", "restored");
        restored.last_restore = Some(serde_json::json!({"summary":"done"}));
        assert!(state.publish(&operation, restored));
        state.end(&operation);
        let (revision, _) = state.observation();
        assert!(state.publish_observation(revision, Status::new("ready", "ready")));
        let status = state.snapshot();
        assert!(status.last_restore.is_some());
        assert!(crate::call_mode_status::valid_public_status(
            &serde_json::to_value(status).unwrap()
        ));
    }
}
