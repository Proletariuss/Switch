//! Module USB/voice backup listing and durable file lifecycle.
//!
//! Rust owns backup storage, public file operations and the native restore
//! workflow; historical backup formats remain readable.

use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::body::Body;
use hyper::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, HeaderValue,
    IF_MATCH, IF_MODIFIED_SINCE, IF_NONE_MATCH, IF_RANGE, IF_UNMODIFIED_SINCE, LAST_MODIFIED,
    RANGE,
};
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use proletarius_store::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::archive_io;
use crate::daemon_lifetime::RootProof;
use crate::startup_recovery::reconcile::backup_lifecycle::{self, BackupWriteKind};

#[path = "call_mode_file_lock.rs"]
mod file_lock;
pub(crate) use file_lock::{
    Kind as RecoveryLockKind, acquire_bound as acquire_bound_recovery_lock,
    acquire_bound_existing as acquire_bound_existing_recovery_lock,
};
pub(crate) use file_lock::{OperationFileLock, OperationNamespaceWitness};
#[cfg(test)]
#[path = "call_mode_backup_create.rs"]
mod backup_create;
#[cfg(test)]
#[path = "call_mode_backups/legacy_authority.rs"]
mod legacy_authority;
#[cfg(test)]
use legacy_authority::{
    handle_delete_public, handle_internal, handle_public, write_imported_backup,
};

const BACKUP_DIRECTORY: &str = "module-backups";
const MAX_BACKUP_BYTES: u64 = 64 * 1024;
const CURRENT_SCHEMA: i64 = 3;
const PREVIOUS_SCHEMA: i64 = 2;
const BACKUP_KIND: &str = "proletarius-usb-composition-backup";
const MAX_DELETE_REQUEST_BYTES: usize = 1024 * 1024;
pub(crate) const INTERNAL_CREATE_PATH: &str = "/api/internal/call-mode/backups/create";
pub(crate) const INTERNAL_RESTORE_DATA_PATH: &str = "/api/internal/call-mode/backups/restore-data";
pub(crate) const INTERNAL_VALIDATE_RESTORE_PATH: &str =
    "/api/internal/call-mode/backups/validate-restore";
const MAX_RESTORE_REQUEST_BYTES: usize = 1024;

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct BackupFile {
    #[serde(deserialize_with = "deserialize_null_i64")]
    schema_version: i64,
    kind: Option<String>,
    reason: Option<String>,
    saved_at: Option<String>,
    module: Option<ModuleIdentity>,
    usb: Option<UsbComposition>,
    voice: Option<VoiceConfiguration>,
    restore_command: Option<String>,
}

fn deserialize_null_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<i64>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct ModuleIdentity {
    imei: Option<String>,
    firmware: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct UsbComposition {
    vendor_id: Option<i64>,
    product_id: Option<i64>,
    flags: Option<Vec<i64>>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct VoiceConfiguration {
    ims: Option<i64>,
    volte_capability: Option<i64>,
    volte_disabled: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteRequest {
    confirm: Option<bool>,
    backup_id: Option<String>,
}

enum DeleteOutcome {
    Deleted,
    ReadFailed(String),
    Busy,
    RemoveFailed(String),
    LockFailed(String),
}

#[derive(Debug)]
pub(crate) enum OperationFileLockError {
    Busy,
    Io(std::io::Error),
}

#[derive(Debug, Serialize, PartialEq)]
struct BackupSummary {
    id: String,
    file_name: String,
    schema_version: i64,
    saved_at: String,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    module_imei: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    firmware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vendor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    product_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<Vec<i64>>,
    adb_enabled: bool,
    uac_enabled: bool,
    voice_included: bool,
    ims_configured: bool,
    imported: bool,
    valid: bool,
    restorable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

pub(crate) fn valid_backup_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 220 || !id.ends_with(".json") {
        return false;
    }
    if !(id.starts_with("usb-before-call-mode-")
        || id.starts_with("usb-before-restore-")
        || id.starts_with("usb-imported-"))
    {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'.' | b'_'))
}

fn backup_directory(data_root: &Path) -> PathBuf {
    data_root.join(BACKUP_DIRECTORY)
}

fn timestamp(value: &str) -> Result<OffsetDateTime, String> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|error| error.to_string())
}

fn formatted_timestamp(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

fn file_modified_at(metadata: &fs::Metadata) -> OffsetDateTime {
    metadata
        .modified()
        .ok()
        .and_then(|modified| {
            let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
            let nanos = i128::from(duration.as_secs()) * 1_000_000_000
                + i128::from(duration.subsec_nanos());
            OffsetDateTime::from_unix_timestamp_nanos(nanos).ok()
        })
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn read_backup(path: &Path) -> Result<(Vec<u8>, fs::Metadata), std::io::Error> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    read_backup_handle(file)
}

fn read_backup_handle(mut file: fs::File) -> Result<(Vec<u8>, fs::Metadata), std::io::Error> {
    read_backup_handle_ref(&mut file)
}

fn read_backup_handle_ref(file: &mut fs::File) -> Result<(Vec<u8>, fs::Metadata), std::io::Error> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BACKUP_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "备份文件类型或大小无效",
        ));
    }
    let mut payload = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(file)
        .take(MAX_BACKUP_BYTES + 1)
        .read_to_end(&mut payload)?;
    if payload.is_empty() || payload.len() as u64 > MAX_BACKUP_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "备份文件类型或大小无效",
        ));
    }
    Ok((payload, metadata))
}

fn summarize(
    id: &str,
    modified_at: OffsetDateTime,
    payload: &[u8],
) -> (BackupSummary, OffsetDateTime) {
    let mut summary = BackupSummary {
        id: id.into(),
        file_name: id.into(),
        schema_version: 0,
        saved_at: formatted_timestamp(modified_at),
        reason: String::new(),
        module_imei: None,
        firmware: None,
        vendor_id: None,
        product_id: None,
        flags: None,
        adb_enabled: false,
        uac_enabled: false,
        voice_included: false,
        ims_configured: false,
        imported: id.starts_with("usb-imported-"),
        valid: false,
        restorable: false,
        detail: None,
    };

    let decoded: Result<Option<BackupFile>, String> = serde_json::from_slice(payload)
        .map_err(|error| error.to_string())
        .and_then(|decoded: Option<BackupFile>| {
            if let Some(saved_at) = decoded
                .as_ref()
                .and_then(|backup| backup.saved_at.as_deref())
            {
                timestamp(saved_at).map(|_| decoded)
            } else {
                Ok(decoded)
            }
        });
    let backup = match decoded {
        Ok(Some(backup)) => backup,
        Ok(None) => BackupFile::default(),
        Err(error) => {
            summary.detail = Some(format!("无法解析备份：{error}"));
            return (summary, modified_at);
        }
    };

    summary.schema_version = backup.schema_version;
    summary.reason = backup.reason.clone().unwrap_or_default();
    let saved_at = match backup.saved_at.as_deref() {
        Some(value) => match timestamp(value) {
            Ok(parsed) => {
                summary.saved_at = formatted_timestamp(parsed);
                parsed
            }
            Err(error) => {
                summary.detail = Some(format!("无法解析备份：{error}"));
                return (summary, modified_at);
            }
        },
        None => {
            let zero = timestamp("0001-01-01T00:00:00Z").unwrap_or(OffsetDateTime::UNIX_EPOCH);
            summary.saved_at = "0001-01-01T00:00:00Z".into();
            zero
        }
    };
    let module = backup.module.unwrap_or_default();
    let usb = backup.usb.unwrap_or_default();
    let voice = backup.voice;
    let vendor_id = usb.vendor_id.unwrap_or_default();
    let product_id = usb.product_id.unwrap_or_default();
    let flags = usb.flags.unwrap_or_default();
    let imei = module.imei.unwrap_or_default();
    let firmware = module.firmware.unwrap_or_default();
    let ims = voice
        .as_ref()
        .and_then(|value| value.ims)
        .unwrap_or_default();
    let volte_capability = voice
        .as_ref()
        .and_then(|value| value.volte_capability)
        .unwrap_or_default();
    let volte_disabled = voice
        .as_ref()
        .and_then(|value| value.volte_disabled)
        .unwrap_or_default();

    summary.module_imei = (!imei.is_empty()).then_some(imei.clone());
    summary.firmware = (!firmware.is_empty()).then_some(firmware);
    summary.vendor_id = Some(format!("0x{:04X}", vendor_id));
    summary.product_id = Some(format!("0x{:04X}", product_id));
    summary.flags = (!flags.is_empty()).then_some(flags.clone());
    summary.adb_enabled = flags.len() >= 2 && flags[flags.len() - 2] == 1;
    summary.uac_enabled = flags.last() == Some(&1);
    summary.voice_included = backup.schema_version != PREVIOUS_SCHEMA && voice.is_some();
    summary.ims_configured =
        summary.voice_included && ims == 1 && volte_capability == 1 && volte_disabled == 0;

    let validation_error = validate_backup(
        backup.schema_version,
        backup.kind.as_deref().unwrap_or_default(),
        backup.reason.as_deref().unwrap_or_default(),
        &imei,
        vendor_id,
        product_id,
        &flags,
        backup.restore_command.as_deref().unwrap_or_default(),
        voice.as_ref(),
    );
    if let Some(error) = validation_error {
        summary.detail = Some(error);
        return (summary, saved_at);
    }
    summary.valid = true;
    if backup.schema_version == 0 {
        summary.detail = Some("旧版备份不包含模块身份，仅支持导出保存".into());
        return (summary, saved_at);
    }
    summary.restorable = true;
    if backup.schema_version == PREVIOUS_SCHEMA {
        summary.detail = Some("此备份只包含 USB 配置；还原时不会更改 IMS/VoLTE。".into());
    }
    (summary, saved_at)
}

#[allow(clippy::too_many_arguments)]
fn validate_backup(
    schema_version: i64,
    kind: &str,
    reason: &str,
    imei: &str,
    vendor_id: i64,
    product_id: i64,
    flags: &[i64],
    restore_command: &str,
    voice: Option<&VoiceConfiguration>,
) -> Option<String> {
    if vendor_id <= 0 || vendor_id > 0xffff || product_id <= 0 || product_id > 0xffff {
        return Some(
            "备份 USB 配置无效：模块 USB VID/PID 超出有效范围；为避免覆盖未知配置，已停止".into(),
        );
    }
    if flags.len() != 7 {
        return Some(format!(
            "备份 USB 配置无效：模块返回了 {} 个 USB 功能位，预期为 7；为避免覆盖未知配置，已停止",
            flags.len()
        ));
    }
    if flags.iter().any(|flag| !matches!(flag, 0 | 1)) {
        return Some(
            "备份 USB 配置无效：模块 USB 配置包含非布尔功能位；为避免覆盖未知配置，已停止".into(),
        );
    }
    let command = format!(
        "AT+QCFG=\"USBCFG\",0x{vendor_id:04X},0x{product_id:04X},{}",
        flags
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    if restore_command != command {
        return Some("备份中的还原命令与结构化 USB 配置不一致".into());
    }
    if schema_version == 0 {
        return None;
    }
    if !matches!(schema_version, PREVIOUS_SCHEMA | CURRENT_SCHEMA) || kind != BACKUP_KIND {
        return Some(format!("不支持的备份格式版本 {schema_version}"));
    }
    if !matches!(reason, "before_call_mode" | "before_restore") {
        return Some("备份原因字段无效".into());
    }
    if imei.len() != 15 || !imei.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some("备份缺少有效的模块 IMEI".into());
    }
    if schema_version == CURRENT_SCHEMA {
        let Some(voice) = voice else {
            return Some("备份缺少 IMS/VoLTE 配置".into());
        };
        let ims = voice.ims.unwrap_or_default();
        let capability = voice.volte_capability.unwrap_or_default();
        let disabled = voice.volte_disabled.unwrap_or_default();
        if !(0..=2).contains(&ims) {
            return Some(format!("IMS 配置值 {ims} 无效"));
        }
        if !matches!(capability, 0 | 1) {
            return Some(format!("VoLTE 能力值 {capability} 无效"));
        }
        if !matches!(disabled, 0 | 1) {
            return Some(format!("VoLTE 开关值 {disabled} 无效"));
        }
    }
    None
}

fn list_backups(data_root: &Path) -> Result<Vec<BackupSummary>, StoreError> {
    let directory = backup_directory(data_root);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(StoreError::Io(error)),
    };
    let mut entries = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Io)?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut backups = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !valid_backup_id(&id) {
            continue;
        }
        let path = directory.join(&id);
        let Ok((payload, metadata)) = read_backup(&path) else {
            continue;
        };
        let modified_at = file_modified_at(&metadata);
        let (summary, sort_time) = summarize(&id, modified_at, &payload);
        backups.push((summary, sort_time));
    }
    backups.sort_by(|left, right| right.1.cmp(&left.1));
    Ok(backups.into_iter().map(|(summary, _)| summary).collect())
}

fn legacy_io_message(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "no such file or directory".into(),
        std::io::ErrorKind::PermissionDenied => "permission denied".into(),
        std::io::ErrorKind::AlreadyExists => "file exists".into(),
        _ => error.to_string(),
    }
}

fn read_export(data_root: &Path, id: &str) -> Result<(Vec<u8>, SystemTime), String> {
    let (payload, metadata, _file) = read_export_with_metadata(data_root, id)?;
    Ok((
        payload,
        metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
    ))
}

fn read_export_with_metadata(
    data_root: &Path,
    id: &str,
) -> Result<(Vec<u8>, fs::Metadata, fs::File), String> {
    if !valid_backup_id(id) {
        return Err("备份文件标识无效".into());
    }
    let path = backup_directory(data_root).join(id);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("lstat {}: {}", path.display(), legacy_io_message(&error)))?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > MAX_BACKUP_BYTES {
        return Err("备份文件类型或大小无效".into());
    }
    let opened = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&path);
    let (payload, opened_metadata, file) = opened
        .and_then(|mut file| {
            let (raw, metadata) = read_backup_handle_ref(&mut file)?;
            Ok((raw, metadata, file))
        })
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData
                || error.raw_os_error() == Some(nix::libc::ELOOP)
            {
                "备份文件类型或大小无效".into()
            } else {
                format!("open {}: {}", path.display(), legacy_io_message(&error))
            }
        })?;
    Ok((payload, opened_metadata, file))
}

pub(crate) fn try_call_mode_operation_file_lock(
    data_root: &Path,
) -> Result<OperationFileLock, OperationFileLockError> {
    file_lock::acquire(data_root, file_lock::Kind::Operation, false).map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            OperationFileLockError::Busy
        } else {
            OperationFileLockError::Io(error)
        }
    })
}

/// Acquires a shared call-mode operation guard. Go call-mode mutations hold
/// the same lock exclusively; Rust voice-route startup holds this guard from
/// readiness probing through completion of its detached ADB worker.
pub(crate) fn try_call_mode_operation_shared_lock(
    data_root: &Path,
) -> Result<OperationFileLock, std::io::Error> {
    file_lock::acquire(data_root, file_lock::Kind::Operation, true)
}

pub(crate) fn try_adb_interface_file_lock(
    data_root: &Path,
) -> Result<OperationFileLock, std::io::Error> {
    file_lock::acquire(data_root, file_lock::Kind::AdbInterface, false)
}

#[derive(Debug)]
enum ImportFailure {
    BadRequest(String),
    Verification(String),
}

enum CreateFailure {
    BadRequest(String),
    Storage(String),
}

fn validate_import_payload(payload: &[u8]) -> Result<BackupFile, String> {
    if payload.is_empty() || payload.len() as u64 > MAX_BACKUP_BYTES {
        return Err("备份文件为空或超过 64 KB".into());
    }
    let parsed: Option<BackupFile> =
        serde_json::from_slice(payload).map_err(|error| format!("解析备份：{error}"))?;
    let mut backup = parsed.unwrap_or_default();
    let Some(saved_at) = backup.saved_at.as_deref() else {
        return Err("备份缺少保存时间".into());
    };
    let saved_at = timestamp(saved_at).map_err(|error| format!("解析备份：{error}"))?;
    if saved_at.year() == 1
        && saved_at.month() as u8 == 1
        && saved_at.day() == 1
        && saved_at.hour() == 0
        && saved_at.minute() == 0
        && saved_at.second() == 0
        && saved_at.nanosecond() == 0
    {
        return Err("备份缺少保存时间".into());
    }
    let module_imei = backup
        .module
        .as_ref()
        .and_then(|module| module.imei.as_deref())
        .unwrap_or_default();
    let usb = backup.usb.as_ref();
    let flags = usb.and_then(|usb| usb.flags.as_deref()).unwrap_or_default();
    if let Some(error) = validate_backup(
        backup.schema_version,
        backup.kind.as_deref().unwrap_or_default(),
        backup.reason.as_deref().unwrap_or_default(),
        module_imei,
        usb.and_then(|usb| usb.vendor_id).unwrap_or_default(),
        usb.and_then(|usb| usb.product_id).unwrap_or_default(),
        flags,
        backup.restore_command.as_deref().unwrap_or_default(),
        backup.voice.as_ref(),
    ) {
        return Err(error);
    }
    // Schema 2 authorizes USB restoration only. An extra voice object is
    // retained in the raw file/export but never becomes a restore target.
    if backup.schema_version == PREVIOUS_SCHEMA {
        backup.voice = None;
    }
    Ok(backup)
}

fn imported_backup_id(backup: &BackupFile, now: OffsetDateTime, index: usize) -> String {
    let imei = backup
        .module
        .as_ref()
        .and_then(|module| module.imei.as_deref())
        .filter(|imei| imei.len() == 15 && imei.bytes().all(|byte| byte.is_ascii_digit()));
    let suffix = imei.map(|imei| &imei[11..]).unwrap_or("legacy");
    let base = format!(
        "usb-imported-{:04}{:02}{:02}-{:02}{:02}{:02}.{:09}-{suffix}",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.nanosecond()
    );
    if index == 1 {
        format!("{base}.json")
    } else {
        format!("{base}-{index}.json")
    }
}

fn created_backup_id(backup: &BackupFile) -> Result<String, String> {
    let saved_at = backup
        .saved_at
        .as_deref()
        .ok_or_else(|| "备份缺少保存时间".to_string())
        .and_then(timestamp)?;
    let reason = match backup.reason.as_deref() {
        Some("before_call_mode") => "call-mode",
        Some("before_restore") => "restore",
        _ => return Err("备份原因字段无效".into()),
    };
    let imei = backup
        .module
        .as_ref()
        .and_then(|module| module.imei.as_deref())
        .filter(|imei| imei.len() == 15 && imei.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(|| "备份缺少有效的模块 IMEI".to_string())?;
    let suffix = &imei[imei.len() - 4..];
    Ok(format!(
        "usb-before-{reason}-{:04}{:02}{:02}-{:02}{:02}{:02}.{:09}-{suffix}.json",
        saved_at.year(),
        saved_at.month() as u8,
        saved_at.day(),
        saved_at.hour(),
        saved_at.minute(),
        saved_at.second(),
        saved_at.nanosecond()
    ))
}

fn created_backup_payload_id(payload: &[u8]) -> Result<String, CreateFailure> {
    let backup = validate_import_payload(payload).map_err(CreateFailure::BadRequest)?;
    if backup.schema_version != CURRENT_SCHEMA {
        return Err(CreateFailure::BadRequest(
            "新建备份必须使用当前格式版本".into(),
        ));
    }
    created_backup_id(&backup).map_err(CreateFailure::BadRequest)
}

fn root_proof_error() -> String {
    "模块备份根目录授权已失效或不匹配".into()
}

fn write_created_backup_with_proof(
    data_root: &Path,
    proof: Result<RootProof, ()>,
    payload: &[u8],
) -> Result<String, CreateFailure> {
    // Payload validation retains precedence over authority and filesystem work.
    let id = created_backup_payload_id(payload)?;
    let proof = proof.map_err(|_| CreateFailure::Storage(root_proof_error()))?;
    backup_lifecycle::prepare_create(data_root, &proof, &id, BackupWriteKind::Create, payload)
        .and_then(|permit| permit.commit())
        .map_err(|error| CreateFailure::Storage(error.to_string()))?;
    Ok(id)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreDataRequest {
    backup_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreBindingRequest {
    backup_id: String,
    module_imei: String,
    vendor_id: i64,
    product_id: i64,
}

fn restore_binding_error(
    backup: &BackupFile,
    current_imei: &str,
    vendor_id: i64,
    product_id: i64,
) -> Option<&'static str> {
    if current_imei.len() != 15 || !current_imei.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some("备份所属模块与当前连接模块的 IMEI 不一致");
    }
    if backup
        .module
        .as_ref()
        .and_then(|module| module.imei.as_deref())
        != Some(current_imei)
    {
        return Some("备份所属模块与当前连接模块的 IMEI 不一致");
    }
    let Some(usb) = backup.usb.as_ref() else {
        return Some("备份与当前模块的 USB VID/PID 不一致");
    };
    if usb.vendor_id != Some(vendor_id) || usb.product_id != Some(product_id) {
        return Some("备份与当前模块的 USB VID/PID 不一致");
    }
    None
}

fn read_restorable_backup(data_root: &Path, id: &str) -> Result<BackupFile, String> {
    if !valid_backup_id(id) {
        return Err("备份文件标识无效".into());
    }
    let path = backup_directory(data_root).join(id);
    let (payload, metadata) = read_backup(&path).map_err(restore_read_error)?;
    let backup = validate_import_payload(&payload)?;
    let modified = file_modified_at(&metadata);
    let (summary, _) = summarize(id, modified, &payload);
    if !summary.restorable {
        return Err(summary
            .detail
            .unwrap_or_else(|| "此备份不能用于自动还原".into()));
    }
    Ok(backup)
}

fn restore_read_error(error: std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::InvalidData
        || error.raw_os_error() == Some(nix::libc::ELOOP)
    {
        "备份文件类型或大小无效".into()
    } else {
        format!("读取备份文件失败：{}", legacy_io_message(&error))
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct NativeBackup {
    pub(crate) saved_at: String,
    pub(crate) module: crate::module_radio_transport::CallModeModuleIdentity,
    pub(crate) usb: crate::module_radio_transport::UsbComposition,
    pub(crate) voice: Option<crate::module_radio_transport::VoiceConfiguration>,
}

/// A restore uses the exact validated input captured before admission. This
/// private in-process value is not a device/recovery permit or an HTTP token.
pub(crate) struct CapturedNativeBackup {
    root: PathBuf,
    root_proof: crate::daemon_lifetime::RootProof,
    id: String,
    root_identity: BackupIdentity,
    directory_identity: BackupIdentity,
    file_identity: BackupIdentity,
    digest: [u8; 32],
    backup: NativeBackup,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct BackupIdentity {
    device: u64,
    inode: u64,
}

impl BackupIdentity {
    fn of(metadata: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

impl CapturedNativeBackup {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    pub(crate) fn backup(&self) -> &NativeBackup {
        &self.backup
    }

    /// A receiving owner must prove that the captured input belongs to its
    /// own actual lifetime, rather than merely to some still-valid lease.
    pub(crate) async fn verify_for_root(
        &self,
        root: &Path,
        proof: &crate::daemon_lifetime::RootProof,
    ) -> Result<(), String> {
        if !self.root_proof.same_authority(proof) {
            return Err("模块备份根目录授权已失效或不匹配：备份不属于当前后端授权".into());
        }
        proof
            .with_root(root, |_| ())
            .map_err(|error| format!("模块备份根目录授权已失效或不匹配：{error}"))?;
        self.verify_unchanged().await?;
        proof
            .with_root(root, |_| ())
            .map_err(|error| format!("模块备份根目录授权已失效或不匹配：{error}"))
    }
    pub(crate) fn sha256(&self) -> String {
        use std::fmt::Write;
        let mut value = String::with_capacity(64);
        for byte in self.digest {
            let _ = write!(value, "{byte:02x}");
        }
        value
    }

    /// Recheck the selected file before reservation and before restore work.
    /// A changed name/inode/content never replaces the captured target values.
    pub(crate) async fn verify_unchanged(&self) -> Result<(), String> {
        let root = self.root.clone();
        let proof = self.root_proof.clone();
        let id = self.id.clone();
        let expected = (
            self.root_identity,
            self.directory_identity,
            self.file_identity,
            self.digest,
        );
        archive_io::run(move || {
            Ok::<_, StoreError>(
                read_pinned_restore_input(&root, &id, &proof).and_then(|current| {
                    if (
                        current.root,
                        current.directory,
                        current.file,
                        backup_digest(&current.payload),
                    ) == expected
                    {
                        Ok(())
                    } else {
                        Err("备份已变化，请重新选择并确认还原".into())
                    }
                }),
            )
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

struct PinnedRestoreInput {
    root: BackupIdentity,
    directory: BackupIdentity,
    file: BackupIdentity,
    payload: Vec<u8>,
    metadata: fs::Metadata,
}

fn backup_digest(payload: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(payload).into()
}

fn read_pinned_restore_input(
    root: &Path,
    id: &str,
    proof: &crate::daemon_lifetime::RootProof,
) -> Result<PinnedRestoreInput, String> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::Mode;
    use std::os::unix::fs::MetadataExt;
    if !valid_backup_id(id) {
        return Err("备份文件标识无效".into());
    }
    let read = |root_file: &fs::File| -> std::io::Result<PinnedRestoreInput> {
        let directory = fs::File::from(openat(
            root_file,
            BACKUP_DIRECTORY,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        let file = fs::File::from(openat(
            &directory,
            id,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
            Mode::empty(),
        )?);
        let file_metadata = file.metadata()?;
        if file_metadata.nlink() != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "备份文件链接状态无效",
            ));
        }
        let (payload, _) = read_backup_handle(file.try_clone()?)?;
        let metadata = file.metadata()?;
        if BackupIdentity::of(&file_metadata) != BackupIdentity::of(&metadata)
            || metadata.len() != payload.len() as u64
            || metadata.nlink() != 1
            || file_metadata.len() != metadata.len()
            || file_metadata.mtime() != metadata.mtime()
            || file_metadata.mtime_nsec() != metadata.mtime_nsec()
            || file_metadata.ctime() != metadata.ctime()
            || file_metadata.ctime_nsec() != metadata.ctime_nsec()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "备份读取期间已变化",
            ));
        }
        Ok(PinnedRestoreInput {
            root: BackupIdentity::of(&root_file.metadata()?),
            directory: BackupIdentity::of(&directory.metadata()?),
            file: BackupIdentity::of(&metadata),
            payload,
            metadata,
        })
    };
    proof
        .with_root(root, read)
        .map_err(|error| format!("模块备份根目录授权已失效或不匹配：{error}"))?
        .map_err(restore_read_error)
}

pub(crate) async fn capture_native_backup(
    root: &Path,
    id: &str,
    proof: &crate::daemon_lifetime::RootProof,
) -> Result<CapturedNativeBackup, String> {
    let root = root.to_path_buf();
    let id = id.to_owned();
    let proof = proof.clone();
    archive_io::run(move || {
        Ok::<_, StoreError>((|| {
            let input = read_pinned_restore_input(&root, &id, &proof)?;
            let backup = validate_import_payload(&input.payload)?;
            let (summary, _) = summarize(&id, file_modified_at(&input.metadata), &input.payload);
            if !summary.restorable {
                return Err(summary
                    .detail
                    .unwrap_or_else(|| "此备份不能用于自动还原".into()));
            }
            let backup = native_backup(backup)?;
            Ok(CapturedNativeBackup {
                root,
                root_proof: proof,
                id,
                root_identity: input.root,
                directory_identity: input.directory,
                file_identity: input.file,
                digest: backup_digest(&input.payload),
                backup,
            })
        })())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn native_backup(backup: BackupFile) -> Result<NativeBackup, String> {
    let mut value = serde_json::to_value(backup).map_err(|error| error.to_string())?;
    // Optional legacy firmware remains display metadata, never binding identity.
    if value["module"]["firmware"].is_null() {
        value["module"]["firmware"] = json!("");
    }
    serde_json::from_value(value).map_err(|error| error.to_string())
}

pub(crate) async fn read_native_backup(data_root: &Path, id: &str) -> Result<NativeBackup, String> {
    let root = data_root.to_path_buf();
    let id = id.to_owned();
    archive_io::run(move || {
        Ok::<_, StoreError>((|| {
            let backup = read_restorable_backup(&root, &id)?;
            native_backup(backup)
        })())
    })
    .await
    .map_err(|error| error.to_string())?
}

pub(crate) async fn create_native_backup(
    data_root: &Path,
    identity: crate::module_radio_transport::CallModeModuleIdentity,
    usb: crate::module_radio_transport::UsbComposition,
    voice: crate::module_radio_transport::VoiceConfiguration,
    before_restore: bool,
    proof: &crate::daemon_lifetime::RootProof,
) -> Result<String, String> {
    let root = data_root.to_path_buf();
    let proof = proof.clone();
    let flags = usb
        .flags
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let payload = serde_json::to_vec(&json!({
        "schema_version": CURRENT_SCHEMA, "kind": BACKUP_KIND,
        "reason": if before_restore { "before_restore" } else { "before_call_mode" },
        "saved_at": formatted_timestamp(OffsetDateTime::now_utc()),
        "module": identity, "usb": usb, "voice": voice,
        "restore_command": format!("AT+QCFG=\"USBCFG\",0x{:04X},0x{:04X},{flags}", usb.vendor_id, usb.product_id),
    })).map_err(|error| error.to_string())?;
    archive_io::run(move || {
        // Preserve the established native RootProof rejection before entering
        // the lifecycle core, which rechecks the same retained root authority.
        let created = proof
            .with_root(&root, |_| ())
            .map_err(|error| {
                CreateFailure::Storage(format!("模块备份根目录授权已失效或不匹配：{error}"))
            })
            .and_then(|()| write_created_backup_with_proof(&root, Ok(proof.clone()), &payload));
        Ok::<_, StoreError>(created.map_err(|error| match error {
            CreateFailure::BadRequest(error) | CreateFailure::Storage(error) => error,
        }))
    })
    .await
    .map_err(|error| error.to_string())?
}

pub(crate) async fn handle_internal_with_proof<B>(
    request: Request<B>,
    data_root: &Path,
    proof: Result<RootProof, ()>,
) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let path = request.uri().path();
    let is_create = path == INTERNAL_CREATE_PATH;
    let is_restore_data = path == INTERNAL_RESTORE_DATA_PATH;
    let is_validate_restore = path == INTERNAL_VALIDATE_RESTORE_PATH;
    if request.method() != Method::POST || (!is_create && !is_restore_data && !is_validate_restore)
    {
        return crate::json_response(StatusCode::NOT_FOUND, &json!({"error":"route not found"}));
    }
    let limit = if is_create {
        MAX_BACKUP_BYTES as usize
    } else {
        MAX_RESTORE_REQUEST_BYTES
    };
    let body = match crate::http_server::collect_body(std::pin::pin!(request.into_body()), limit)
        .await
    {
        Ok(body) => body,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Err(crate::http_server::BodyReadError::TooLarge) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":if is_create { "保存备份失败：http: request body too large" } else { "读取备份失败：http: request body too large" }}),
            );
        }
        Err(crate::http_server::BodyReadError::Invalid) => {
            let error = "invalid request body";
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":format!("{}：{error}", if is_create { "保存备份失败" } else { "读取备份失败" })}),
            );
        }
    };
    let root = data_root.to_path_buf();
    if is_create {
        return match archive_io::run(move || {
            Ok::<_, StoreError>(write_created_backup_with_proof(&root, proof, &body))
        })
        .await
        {
            Ok(Ok(id)) => crate::json_response(StatusCode::CREATED, &json!({"backup_id":id})),
            Ok(Err(CreateFailure::BadRequest(error))) => crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":format!("保存备份失败：{error}")}),
            ),
            Ok(Err(CreateFailure::Storage(error))) => crate::json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"error":format!("保存备份失败：{error}")}),
            ),
            Err(error) => crate::json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"error":format!("保存备份失败：{error}")}),
            ),
        };
    }

    if is_validate_restore {
        let request: RestoreBindingRequest = match serde_json::from_slice(&body) {
            Ok(request) => request,
            Err(error) => {
                return crate::json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":format!("还原备份绑定校验请求无效：{error}")}),
                );
            }
        };
        let id = request.backup_id.trim().to_owned();
        let current_imei = request.module_imei.trim().to_owned();
        return match archive_io::run(move || {
            let result = read_restorable_backup(&root, &id).and_then(|backup| {
                restore_binding_error(
                    &backup,
                    &current_imei,
                    request.vendor_id,
                    request.product_id,
                )
                .map_or(Ok(()), |error| Err(error.to_owned()))
            });
            Ok::<_, StoreError>(result)
        })
        .await
        {
            Ok(Ok(())) => crate::json_response(StatusCode::OK, &json!({"ok":true})),
            Ok(Err(error)) => {
                crate::json_response(StatusCode::BAD_REQUEST, &json!({"error":error}))
            }
            Err(error) => crate::json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"error":format!("还原备份绑定校验失败：{error}")}),
            ),
        };
    }

    let request: RestoreDataRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":format!("读取备份失败：无效请求：{error}")}),
            );
        }
    };
    let id = request.backup_id.trim().to_owned();
    match archive_io::run(move || Ok::<_, StoreError>(read_restorable_backup(&root, &id))).await {
        Ok(Ok(backup)) => crate::json_response(StatusCode::OK, &backup),
        Ok(Err(error)) => crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":format!("读取备份失败：{error}")}),
        ),
        Err(error) => crate::json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"error":format!("读取备份失败：{error}")}),
        ),
    }
}

fn write_imported_backup_with_proof(
    data_root: &Path,
    proof: Result<RootProof, ()>,
    payload: &[u8],
    now: OffsetDateTime,
) -> Result<BackupSummary, ImportFailure> {
    let backup = validate_import_payload(payload).map_err(ImportFailure::BadRequest)?;
    let proof = proof.map_err(|_| ImportFailure::BadRequest(root_proof_error()))?;
    for index in 1..=100 {
        let id = imported_backup_id(&backup, now, index);
        let permit = match backup_lifecycle::prepare_create(
            data_root,
            &proof,
            &id,
            BackupWriteKind::Import,
            payload,
        ) {
            Ok(permit) => permit,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(ImportFailure::BadRequest(error.to_string())),
        };
        // Once a durable Prepare exists, a commit error is never a collision
        // retry or permission to erase a possibly committed backup.
        permit
            .commit()
            .map_err(|error| ImportFailure::BadRequest(error.to_string()))?;
        let (stored_payload, modified) =
            read_export(data_root, &id).map_err(ImportFailure::Verification)?;
        let (summary, _) = summarize(
            &id,
            file_modified_at_from_system_time(modified),
            &stored_payload,
        );
        return Ok(summary);
    }
    Err(ImportFailure::BadRequest(
        "无法为导入备份分配唯一文件名".into(),
    ))
}

fn file_modified_at_from_system_time(value: SystemTime) -> OffsetDateTime {
    value
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| {
            OffsetDateTime::from_unix_timestamp_nanos(
                i128::from(duration.as_secs()) * 1_000_000_000
                    + i128::from(duration.subsec_nanos()),
            )
            .ok()
        })
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

#[derive(Clone, Copy)]
enum RangeError {
    Invalid,
    NoOverlap,
}

fn parse_byte_ranges(value: &str, size: usize) -> Result<Option<Vec<(usize, usize)>>, RangeError> {
    if value.is_empty() {
        return Ok(None);
    }
    let Some(value) = value.strip_prefix("bytes=") else {
        return Err(RangeError::Invalid);
    };
    let mut ranges = Vec::new();
    let mut no_overlap = false;
    for part in value.split(',').map(str::trim) {
        if part.is_empty() {
            continue;
        }
        let Some((start, end)) = part.split_once('-') else {
            return Err(RangeError::Invalid);
        };
        let start = start.trim();
        let end = end.trim();
        let range = if start.is_empty() {
            if end.is_empty() || end.starts_with('-') {
                return Err(RangeError::Invalid);
            }
            let suffix = end.parse::<i64>().map_err(|_| RangeError::Invalid)?;
            if suffix < 0 {
                return Err(RangeError::Invalid);
            }
            let suffix = suffix as usize;
            let suffix = suffix.min(size);
            (size - suffix, size.saturating_sub(1))
        } else {
            let first = start.parse::<i64>().map_err(|_| RangeError::Invalid)?;
            if first < 0 {
                return Err(RangeError::Invalid);
            }
            let first = first as usize;
            if first >= size {
                no_overlap = true;
                continue;
            }
            let last = if end.is_empty() {
                size - 1
            } else {
                let last = end.parse::<i64>().map_err(|_| RangeError::Invalid)?;
                if last < 0 {
                    return Err(RangeError::Invalid);
                }
                let last = last as usize;
                if first > last {
                    return Err(RangeError::Invalid);
                }
                last.min(size - 1)
            };
            (first, last)
        };
        if size == 0 {
            no_overlap = true;
        } else {
            // Go also retains the zero-length suffix range (bytes=-0).
            ranges.push(range);
        }
    }
    if ranges.is_empty() {
        if no_overlap {
            Err(RangeError::NoOverlap)
        } else {
            Ok(None)
        }
    } else {
        Ok(Some(ranges))
    }
}

fn date_is_at_or_after(value: Option<&HeaderValue>, modified: SystemTime) -> bool {
    if zero_modification_time(modified) {
        return false;
    }
    value
        .and_then(|value| value.to_str().ok())
        .and_then(parse_export_http_date)
        .is_some_and(|date| date_at_or_after(date, modified))
}

fn zero_modification_time(value: SystemTime) -> bool {
    value == SystemTime::UNIX_EPOCH || go_zero_time(value)
}

fn go_zero_time(value: SystemTime) -> bool {
    SystemTime::UNIX_EPOCH
        .checked_sub(std::time::Duration::from_secs(62_135_596_800))
        .is_some_and(|zero| value == zero)
}

fn export_http_date(value: SystemTime) -> Option<String> {
    let second = i64::try_from(unix_second(value)).ok()?;
    let date = OffsetDateTime::from_unix_timestamp(second).ok()?;
    // HTTP's four-digit year. Avoid httpdate's panic on pre-1970/out-of-range
    // file metadata; unsupported extreme dates return an explicit read error.
    if !(0..=9999).contains(&date.year()) {
        return None;
    }
    let weekday = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
        [usize::from(date.weekday().number_days_from_monday())];
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][usize::from(date.month() as u8) - 1];
    Some(format!(
        "{weekday}, {:02} {month} {:04} {:02}:{:02}:{:02} GMT",
        date.day(),
        date.year(),
        date.hour(),
        date.minute(),
        date.second()
    ))
}

fn parse_export_http_date(value: &str) -> Option<SystemTime> {
    // net/http.ParseTime accepts IMF-fixdate, RFC850 and ANSI C asctime.
    // Unlike httpdate 1.0.3 these also support dates before Unix epoch.
    static FORMATS: std::sync::OnceLock<
        [time::format_description::FormatDescriptionV3<'static>; 3],
    > = std::sync::OnceLock::new();
    let formats = FORMATS.get_or_init(|| [
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT",
        "[weekday repr:long], [day]-[month repr:short]-[year] [hour]:[minute]:[second] GMT",
        "[weekday repr:short] [month repr:short] [day padding:space] [hour]:[minute]:[second] [year]",
    ].map(|format| time::format_description::parse_borrowed::<3>(format).expect("constant HTTP date format")));
    // time.Parse accepts a decimal fraction after seconds even when the
    // layout has no fractional component. These preconditions compare whole
    // seconds, so discard a syntactically valid fraction before parsing.
    let fraction = value
        .as_bytes()
        .iter()
        .enumerate()
        .find_map(|(index, &byte)| {
            if !matches!(byte, b'.' | b',')
                || index < 8
                || value.as_bytes()[index - 3] != b':'
                || value.as_bytes()[index - 6] != b':'
                || !value.as_bytes()[index - 1].is_ascii_digit()
            {
                return None;
            }
            let mut end = index + 1;
            while value.as_bytes().get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            (end > index + 1).then(|| format!("{}{}", &value[..index], &value[end..]))
        });
    let value = fraction.as_deref().unwrap_or(value);
    // Go's RFC850 two-digit year pivot is 69, not httpdate's 70.
    let expanded = value.split_once(", ").and_then(|(weekday, rest)| {
        let (date, rest) = rest.split_once(' ')?;
        let parts = date.split('-').collect::<Vec<_>>();
        if parts.len() != 3
            || parts[2].len() != 2
            || !parts[2].bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let year = parts[2].parse::<u16>().ok()?;
        Some(format!(
            "{weekday}, {}-{}-{:04} {rest}",
            parts[0],
            parts[1],
            year + if year >= 69 { 1900 } else { 2000 }
        ))
    });
    let date = time::PlainDateTime::parse(value, &formats[0])
        .ok()
        .or_else(|| {
            expanded
                .as_deref()
                .and_then(|value| time::PlainDateTime::parse(value, &formats[1]).ok())
        })
        .or_else(|| time::PlainDateTime::parse(value, &formats[2]).ok())?;
    let second = date.assume_utc().unix_timestamp();
    if second >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(second as u64))
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(std::time::Duration::from_secs(second.unsigned_abs()))
    }
}

// ServeContent has no application ETag on this endpoint. A syntactically
// reached '*' still matches the existing representation; other tags cannot.
fn wildcard_etag(value: &[u8]) -> bool {
    let mut remaining = value;
    loop {
        remaining = remaining.trim_ascii();
        if let Some(rest) = remaining.strip_prefix(b",") {
            remaining = rest;
            continue;
        }
        if remaining.starts_with(b"*") {
            return true;
        }
        let tag = remaining.strip_prefix(b"W/").unwrap_or(remaining);
        let Some(tag) = tag.strip_prefix(b"\"") else {
            return false;
        };
        let Some(end) = tag
            .iter()
            .position(|&byte| byte == b'"' || byte < 0x21 || byte == 0x7f)
        else {
            return false;
        };
        if tag[end] != b'"' {
            return false;
        }
        remaining = &tag[end + 1..];
    }
}

fn nonempty_header(headers: &HeaderMap, name: hyper::header::HeaderName) -> Option<&HeaderValue> {
    headers.get(name).filter(|value| !value.is_empty())
}

fn date_at_or_after(date: SystemTime, modified: SystemTime) -> bool {
    unix_second(date) >= unix_second(modified)
}

fn unix_second(time: SystemTime) -> i128 {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(value) => i128::from(value.as_secs()),
        Err(error) => {
            -i128::from(error.duration().as_secs())
                - i128::from(error.duration().subsec_nanos() != 0)
        }
    }
}

fn export_error(message: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::NOT_FOUND,
        &json!({"error":format!("读取备份失败：{message}")}),
    )
}

fn range_not_satisfiable(id: &str, size: usize, reason: RangeError) -> Response<Full<Bytes>> {
    let message = match reason {
        RangeError::Invalid => "invalid range\n",
        RangeError::NoOverlap => "invalid range: failed to overlap\n",
    };
    let body = Bytes::copy_from_slice(message.as_bytes());
    let mut builder = Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(CONTENT_LENGTH, body.len().to_string())
        .header("x-content-type-options", "nosniff")
        .header(
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{id}\""),
        );
    // Default Go1.26 ServeContent errors remove Last-Modified and are sent
    // before Accept-Ranges is installed. Only no-overlap sets Content-Range.
    if matches!(reason, RangeError::NoOverlap) {
        builder = builder.header(CONTENT_RANGE, format!("bytes */{size}"));
    }
    builder.body(Full::new(body)).expect("fixed range response")
}

fn export_response(
    id: &str,
    payload: Vec<u8>,
    modified: SystemTime,
    headers: &HeaderMap,
) -> Response<Full<Bytes>> {
    let size = payload.len();
    let mut builder = Response::builder()
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header(
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{id}\""),
        )
        .header("x-content-type-options", "nosniff");
    if !zero_modification_time(modified) {
        let Some(date) = export_http_date(modified) else {
            return export_error("备份文件修改时间超出支持范围");
        };
        builder = builder.header(LAST_MODIFIED, date);
    }
    let if_match = nonempty_header(headers, IF_MATCH);
    let precondition_failed = if let Some(value) = if_match {
        !wildcard_etag(value.as_bytes())
    } else {
        nonempty_header(headers, IF_UNMODIFIED_SINCE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_export_http_date)
            .is_some_and(|date| {
                !zero_modification_time(modified) && !date_at_or_after(date, modified)
            })
    };
    if precondition_failed {
        return builder
            .status(StatusCode::PRECONDITION_FAILED)
            .body(Full::new(Bytes::new()))
            .expect("fixed precondition response");
    }
    let if_none_match = nonempty_header(headers, IF_NONE_MATCH);
    let not_modified = if let Some(value) = if_none_match {
        wildcard_etag(value.as_bytes())
    } else {
        date_is_at_or_after(headers.get(IF_MODIFIED_SINCE), modified)
    };
    if not_modified {
        builder
            .headers_mut()
            .expect("response headers")
            .remove(CONTENT_TYPE);
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(Full::new(Bytes::new()))
            .expect("fixed not-modified response");
    }

    let range_text = headers
        .get(RANGE)
        .map(|value| value.to_str().unwrap_or("\0"))
        .unwrap_or_default();
    let if_range_matches = headers
        .get(IF_RANGE)
        .filter(|value| !value.is_empty())
        .is_none_or(|value| {
            value
                .to_str()
                .ok()
                .and_then(parse_export_http_date)
                .is_some_and(|date| {
                    // Go checkIfRange uses Time.IsZero (year1), whereas
                    // Last-Modified/IMS also treat Unix epoch as unspecified.
                    !go_zero_time(modified) && unix_second(date) == unix_second(modified)
                })
        });
    let mut ranges = if if_range_matches {
        match parse_byte_ranges(range_text, size) {
            Ok(ranges) => ranges,
            Err(error) => return range_not_satisfiable(id, size, error),
        }
    } else {
        None
    };

    if ranges.as_ref().is_some_and(|ranges| {
        ranges.iter().fold(0usize, |sum, (start, end)| {
            sum.saturating_add(end + 1 - start)
        }) > size
    }) {
        ranges = None;
    }
    builder = builder.header(ACCEPT_RANGES, "bytes");

    match ranges {
        None => {
            let body = Bytes::from(payload);
            builder
                .status(StatusCode::OK)
                .header(CONTENT_LENGTH, body.len().to_string())
                .body(Full::new(body))
                .expect("fixed export response")
        }
        Some(ranges) if ranges.len() == 1 => {
            let (start, end) = ranges[0];
            let body = Bytes::copy_from_slice(&payload[start..=end]);
            builder
                .status(StatusCode::PARTIAL_CONTENT)
                .header(CONTENT_RANGE, format!("bytes {start}-{end}/{size}"))
                .header(CONTENT_LENGTH, body.len().to_string())
                .body(Full::new(body))
                .expect("fixed partial export response")
        }
        Some(ranges) => {
            let mut random = [0u8; 12];
            let _ = getrandom::fill(&mut random);
            let entropy: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            let mut boundary = format!("switch-backup-{entropy}");
            while payload
                .windows(boundary.len())
                .any(|window| window == boundary.as_bytes())
            {
                boundary.push('x');
            }
            let mut body = Vec::new();
            for (start, end) in ranges {
                body.extend_from_slice(format!("--{boundary}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Range: bytes {start}-{end}/{size}\r\n\r\n").as_bytes());
                body.extend_from_slice(&payload[start..=end]);
                body.extend_from_slice(b"\r\n");
            }
            body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            let body = Bytes::from(body);
            let mut builder = builder.status(StatusCode::PARTIAL_CONTENT);
            builder.headers_mut().expect("response headers").insert(
                CONTENT_TYPE,
                HeaderValue::from_str(&format!("multipart/byteranges; boundary={boundary}"))
                    .expect("generated multipart content type"),
            );
            builder
                .header(CONTENT_LENGTH, body.len().to_string())
                .body(Full::new(body))
                .expect("fixed multipart export response")
        }
    }
}

pub(crate) async fn handle_delete_public_with_proof<B>(
    request: Request<B>,
    data_root: &Path,
    service: crate::module_radio::Service,
    proof: Result<RootProof, ()>,
) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let body = match crate::http_server::collect_body(
        std::pin::pin!(request.into_body()),
        MAX_DELETE_REQUEST_BYTES,
    )
    .await
    {
        Ok(body) => body,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Err(crate::http_server::BodyReadError::TooLarge) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"http: request body too large"}),
            );
        }
        Err(crate::http_server::BodyReadError::Invalid) => {
            let error = "invalid request body";
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":error.to_string()}),
            );
        }
    };
    let parsed = match serde_json::from_slice::<Option<DeleteRequest>>(&body) {
        Ok(Some(request)) => request,
        Ok(None) => DeleteRequest {
            confirm: None,
            backup_id: None,
        },
        Err(error) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":error.to_string()}),
            );
        }
    };
    if !parsed.confirm.unwrap_or(false) {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":"需要确认后才会删除配置备份"}),
        );
    }
    let id = parsed.backup_id.unwrap_or_default().trim().to_owned();
    let root = data_root.to_path_buf();
    let delete_id = id.clone();
    let result = archive_io::run(move || {
        let path = backup_directory(&root).join(&delete_id);
        let (selected_raw, selected_metadata, selected_file) =
            match read_export_with_metadata(&root, &delete_id) {
                Ok(selected) => selected,
                Err(error) => return Ok::<_, StoreError>(DeleteOutcome::ReadFailed(error)),
            };
        // This is selected file data only. Root authority and the inventory
        // lock are still required by prepare_delete_selected after busy checks.
        let selection = backup_lifecycle::BackupDeleteSelection::capture(
            &selected_file,
            &selected_metadata,
            &selected_raw,
        );
        #[cfg(test)]
        lifecycle_tests::after_delete_selection(&root);
        let Some(_guard) = service.try_call_mode_backup_delete() else {
            return Ok::<_, StoreError>(DeleteOutcome::Busy);
        };
        let operation_file_lock = match try_call_mode_operation_file_lock(&root) {
            Ok(file) => file,
            Err(OperationFileLockError::Busy) => {
                return Ok::<_, StoreError>(DeleteOutcome::Busy);
            }
            Err(OperationFileLockError::Io(error)) => {
                let lock_path = root.join("call-mode-operation.lock");
                return Ok::<_, StoreError>(DeleteOutcome::LockFailed(format!(
                    "open {}: {}",
                    lock_path.display(),
                    legacy_io_message(&error)
                )));
            }
        };
        let proof = match proof {
            Ok(proof) => proof,
            Err(()) => return Ok::<_, StoreError>(DeleteOutcome::RemoveFailed(root_proof_error())),
        };
        let selection = match selection {
            Ok(selection) => selection,
            Err(error) => {
                return Ok::<_, StoreError>(DeleteOutcome::RemoveFailed(error.to_string()));
            }
        };
        match backup_lifecycle::prepare_delete_selected(&root, &proof, &delete_id, &selection)
            .and_then(|permit| permit.commit())
        {
            Ok(()) => {
                drop(operation_file_lock);
                Ok::<_, StoreError>(DeleteOutcome::Deleted)
            }
            Err(error) => Ok::<_, StoreError>(DeleteOutcome::RemoveFailed(format!(
                "remove {}: {}",
                path.display(),
                legacy_io_message(&error)
            ))),
        }
    })
    .await;
    match result {
        Ok(DeleteOutcome::Deleted) => {
            crate::json_response(StatusCode::OK, &json!({"deleted":true,"backup_id":id}))
        }
        Ok(DeleteOutcome::ReadFailed(error)) => crate::json_response(
            StatusCode::NOT_FOUND,
            &json!({"error":format!("读取待删除备份失败：{error}")}),
        ),
        Ok(DeleteOutcome::Busy) => crate::json_response(
            StatusCode::CONFLICT,
            &json!({"error":"通话模式正在执行其他操作，不能删除配置备份"}),
        ),
        Ok(DeleteOutcome::RemoveFailed(error)) => crate::json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"error":format!("删除配置备份失败：{error}")}),
        ),
        Ok(DeleteOutcome::LockFailed(error)) => crate::json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"error":format!("删除配置备份失败：{error}")}),
        ),
        Err(error) => crate::json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"error":format!("删除配置备份失败：{error}")}),
        ),
    }
}

pub(crate) async fn handle_public_with_proof<B>(
    request: Request<B>,
    data_root: &Path,
    proof: Result<RootProof, ()>,
) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if request.method() == Method::POST && request.uri().path() == "/api/call-mode/backups/import" {
        let body = match crate::http_server::collect_body(
            std::pin::pin!(request.into_body()),
            MAX_BACKUP_BYTES as usize,
        )
        .await
        {
            Ok(body) => body,
            Err(crate::http_server::BodyReadError::Timeout) => {
                return crate::http_server::body_timeout_response();
            }
            Err(crate::http_server::BodyReadError::TooLarge) => {
                return crate::json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":"读取导入备份失败：http: request body too large"}),
                );
            }
            Err(crate::http_server::BodyReadError::Invalid) => {
                let error = "invalid request body";
                return crate::json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":format!("读取导入备份失败：{error}")}),
                );
            }
        };
        let root = data_root.to_path_buf();
        let result = archive_io::run(move || {
            let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
            Ok::<_, StoreError>(write_imported_backup_with_proof(&root, proof, &body, now))
        })
        .await;
        return match result {
            Ok(Ok(summary)) => {
                crate::json_response(StatusCode::CREATED, &json!({"backup":summary}))
            }
            Ok(Err(ImportFailure::BadRequest(error))) => crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":format!("导入备份失败：{error}")}),
            ),
            Ok(Err(ImportFailure::Verification(error))) => crate::json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"error":format!("验证导入备份失败：{error}")}),
            ),
            Err(error) => crate::json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"error":format!("导入备份失败：{error}")}),
            ),
        };
    }
    if request.method() != Method::GET {
        return crate::json_response(StatusCode::NOT_FOUND, &json!({"error":"route not found"}));
    }
    if request.uri().path() == "/api/call-mode/backups/export" {
        let id = url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
            .find(|(key, _)| key == "id")
            .map(|(_, value)| value.trim().to_owned())
            .unwrap_or_default();
        let root = data_root.to_path_buf();
        let export_id = id.clone();
        let file =
            archive_io::run(move || Ok::<_, StoreError>(read_export(&root, &export_id))).await;
        return match file {
            Ok(Ok((payload, modified))) => {
                export_response(&id, payload, modified, request.headers())
            }
            Ok(Err(error)) => export_error(&error),
            Err(error) => export_error(&error.to_string()),
        };
    }
    if request.uri().path() != "/api/call-mode/backups" {
        return crate::json_response(StatusCode::NOT_FOUND, &json!({"error":"route not found"}));
    }
    let root = data_root.to_path_buf();
    match crate::archive_io::run(move || list_backups(&root)).await {
        Ok(backups) => crate::json_response(StatusCode::OK, &json!({"backups": backups})),
        Err(error) => crate::json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"error":format!("读取模块配置备份失败：{error}")}),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct FailedIngressBody;
    impl hyper::body::Body for FailedIngressBody {
        type Data = Bytes;
        type Error = std::io::Error;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
            std::task::Poll::Ready(Some(Err(std::io::Error::other(
                "fixture transport failure",
            ))))
        }
    }

    #[tokio::test]
    async fn backup_ingress_invalid_and_too_large_keep_legacy_error_contracts() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../build/runtime/stability-695")
            .join(format!(
                "backup-ingress-{}",
                crate::snapshot::new_instance_id()
            ));
        assert!(!root.exists());
        for (path, limit, prefix) in [
            (
                INTERNAL_CREATE_PATH,
                MAX_BACKUP_BYTES as usize,
                "保存备份失败：",
            ),
            (
                INTERNAL_RESTORE_DATA_PATH,
                MAX_RESTORE_REQUEST_BYTES,
                "读取备份失败：",
            ),
            (
                INTERNAL_VALIDATE_RESTORE_PATH,
                MAX_RESTORE_REQUEST_BYTES,
                "读取备份失败：",
            ),
            (
                "/api/call-mode/backups/import",
                MAX_BACKUP_BYTES as usize,
                "读取导入备份失败：",
            ),
            (
                "/api/call-mode/backups/delete",
                MAX_DELETE_REQUEST_BYTES,
                "",
            ),
        ] {
            let failed = Request::builder()
                .method(Method::POST)
                .uri(path)
                .body(FailedIngressBody)
                .unwrap();
            let response = if path == "/api/call-mode/backups/import" {
                handle_public_with_proof(failed, &root, Err(())).await
            } else if path == "/api/call-mode/backups/delete" {
                handle_delete_public_with_proof(
                    failed,
                    &root,
                    crate::module_radio::tests::tests::Fixture::new()
                        .service
                        .clone(),
                    Err(()),
                )
                .await
            } else {
                handle_internal_with_proof(failed, &root, Err(())).await
            };
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let value: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(value["error"], format!("{prefix}invalid request body"));

            let oversized = Request::builder()
                .method(Method::POST)
                .uri(path)
                .body(Full::new(Bytes::from(vec![0; limit + 1])))
                .unwrap();
            let response = if path == "/api/call-mode/backups/import" {
                handle_public_with_proof(oversized, &root, Err(())).await
            } else if path == "/api/call-mode/backups/delete" {
                handle_delete_public_with_proof(
                    oversized,
                    &root,
                    crate::module_radio::tests::tests::Fixture::new()
                        .service
                        .clone(),
                    Err(()),
                )
                .await
            } else {
                handle_internal_with_proof(oversized, &root, Err(())).await
            };
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let value: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(
                value["error"],
                format!("{prefix}http: request body too large")
            );
            assert!(!root.exists());
        }
    }

    fn test_root() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "switch-call-mode-backups-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn shared_audio_guard_excludes_call_mode_mutations_across_process_lock_contract() {
        let root = test_root();
        fs::create_dir_all(&root).unwrap();
        let operation = try_call_mode_operation_file_lock(&root).unwrap();
        let blocked = try_call_mode_operation_shared_lock(&root).unwrap_err();
        assert_eq!(blocked.kind(), std::io::ErrorKind::WouldBlock);
        drop(operation);

        let audio = try_call_mode_operation_shared_lock(&root).unwrap();
        assert!(matches!(
            try_call_mode_operation_file_lock(&root),
            Err(OperationFileLockError::Busy)
        ));
        drop(audio);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn close_only_control_retains_lock_through_duplicated_file_description() {
        let root = test_root();
        fs::create_dir_all(&root).unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("call-mode-operation.lock"))
            .unwrap();
        file.try_lock().unwrap();
        let duplicate = file.try_clone().unwrap();
        drop(file);
        assert!(matches!(
            try_call_mode_operation_file_lock(&root),
            Err(OperationFileLockError::Busy)
        ));
        // A duplicated descriptor shares the same kernel lock description.
        duplicate.unlock().unwrap();
        let available = try_call_mode_operation_file_lock(&root).unwrap();
        drop(available);
        drop(duplicate);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exclusive_guard_unlocks_only_after_last_arc_even_with_duplicate_descriptor() {
        let root = test_root();
        fs::create_dir_all(&root).unwrap();
        let operation = std::sync::Arc::new(try_call_mode_operation_file_lock(&root).unwrap());
        // Private-field access is confined to this child test module. The
        // production guard exposes neither cloning nor access to its File.
        let duplicate = operation.file.try_clone().unwrap();
        let physical_worker = operation.clone();
        drop(operation);
        assert!(matches!(
            try_call_mode_operation_file_lock(&root),
            Err(OperationFileLockError::Busy)
        ));
        assert_eq!(
            try_call_mode_operation_shared_lock(&root)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(physical_worker);
        // The unrelated duplicate is still open, but the last real owner has
        // explicitly released the old open-file-description lock.
        let replacement = try_call_mode_operation_file_lock(&root).unwrap();
        drop(duplicate);
        assert!(matches!(
            try_call_mode_operation_file_lock(&root),
            Err(OperationFileLockError::Busy)
        ));
        drop(replacement);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shared_guards_keep_independent_owners_and_release_duplicate_descriptors() {
        let root = test_root();
        fs::create_dir_all(&root).unwrap();
        let audio = try_call_mode_operation_shared_lock(&root).unwrap();
        let duplicate = audio.file.try_clone().unwrap();
        let other_audio = try_call_mode_operation_shared_lock(&root).unwrap();
        drop(audio);
        assert!(matches!(
            try_call_mode_operation_file_lock(&root),
            Err(OperationFileLockError::Busy)
        ));
        drop(other_audio);
        // Releasing one shared guard does not release another independently
        // acquired shared lock, and a duplicate cannot extend either owner.
        let operation = try_call_mode_operation_file_lock(&root).unwrap();
        drop(duplicate);
        assert_eq!(
            try_call_mode_operation_shared_lock(&root)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(operation);
        fs::remove_dir_all(root).unwrap();
    }

    fn fixture(saved_at: &str, schema: i64, reason: &str, flags: [i64; 7]) -> Value {
        json!({
            "schema_version":schema,
            "kind":BACKUP_KIND,
            "reason":reason,
            "saved_at":saved_at,
            "module":{"imei":"123456789012345","firmware":"fixture-fw"},
            "usb":{"vendor_id":11388,"product_id":9216,"flags":flags},
            "voice":{"ims":1,"volte_capability":1,"volte_disabled":0},
            "restore_command":format!("AT+QCFG=\"USBCFG\",0x2C7C,0x2400,{}",flags.iter().map(i64::to_string).collect::<Vec<_>>().join(","))
        })
    }

    fn import_request(payload: impl Into<Bytes>) -> Request<Full<Bytes>> {
        Request::builder()
            .method(Method::POST)
            .uri("/api/call-mode/backups/import")
            .body(Full::new(payload.into()))
            .unwrap()
    }

    fn create_request(payload: impl Into<Bytes>) -> Request<Full<Bytes>> {
        Request::builder()
            .method(Method::POST)
            .uri(INTERNAL_CREATE_PATH)
            .body(Full::new(payload.into()))
            .unwrap()
    }

    fn restore_data_request(payload: impl Into<Bytes>) -> Request<Full<Bytes>> {
        Request::builder()
            .method(Method::POST)
            .uri(INTERNAL_RESTORE_DATA_PATH)
            .body(Full::new(payload.into()))
            .unwrap()
    }

    fn validate_restore_request(payload: impl Into<Bytes>) -> Request<Full<Bytes>> {
        Request::builder()
            .method(Method::POST)
            .uri(INTERNAL_VALIDATE_RESTORE_PATH)
            .body(Full::new(payload.into()))
            .unwrap()
    }

    fn delete_request(payload: impl Into<Bytes>) -> Request<Full<Bytes>> {
        Request::builder()
            .method(Method::POST)
            .uri("/api/call-mode/backups/delete")
            .body(Full::new(payload.into()))
            .unwrap()
    }

    fn delete_payload(confirm: bool, id: &str) -> Bytes {
        Bytes::from(serde_json::to_vec(&json!({"confirm":confirm,"backup_id":id})).unwrap())
    }

    #[test]
    fn missing_directory_is_an_empty_snapshot() {
        let root = test_root();
        assert!(list_backups(&root).unwrap().is_empty());
        assert!(
            !root.exists(),
            "read-only listing created its data directory"
        );
    }

    #[test]
    fn list_matches_current_schema_summary_and_newest_first_order() {
        let root = test_root();
        let directory = backup_directory(&root);
        fs::create_dir_all(&directory).unwrap();
        let older_id = "usb-before-call-mode-20261001-100000.000000000-2345.json";
        let newer_id = "usb-before-restore-20261002-100000.000000000-2345.json";
        let flags = [0, 0, 0, 0, 0, 1, 1];
        fs::write(
            directory.join(older_id),
            serde_json::to_vec(&fixture(
                "2026-10-01T10:00:00Z",
                CURRENT_SCHEMA,
                "before_call_mode",
                flags,
            ))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            directory.join(newer_id),
            serde_json::to_vec(&fixture(
                "2026-10-02T10:00:00Z",
                CURRENT_SCHEMA,
                "before_restore",
                flags,
            ))
            .unwrap(),
        )
        .unwrap();
        fs::write(directory.join("other.json"), b"{}").unwrap();
        let malformed_path = directory.join("usb-before-call-mode-broken.json");
        fs::write(&malformed_path, b"{not-json").unwrap();
        OpenOptions::new()
            .write(true)
            .open(&malformed_path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
            .unwrap();

        let backups = list_backups(&root).unwrap();
        assert_eq!(
            backups.len(),
            3,
            "invalid-name entries are ignored; malformed backup files remain visible"
        );
        assert_eq!(backups[0].id, newer_id);
        assert_eq!(backups[0].schema_version, CURRENT_SCHEMA);
        assert!(
            backups[0].valid
                && backups[0].restorable
                && backups[0].uac_enabled
                && backups[0].adb_enabled
        );
        assert!(backups[0].ims_configured && backups[0].voice_included);
        assert_eq!(backups[0].vendor_id.as_deref(), Some("0x2C7C"));
        assert_eq!(backups[0].product_id.as_deref(), Some("0x2400"));
        let malformed = backups
            .iter()
            .find(|backup| backup.id == "usb-before-call-mode-broken.json")
            .unwrap();
        assert!(
            !malformed.valid
                && malformed
                    .detail
                    .as_deref()
                    .unwrap()
                    .starts_with("无法解析备份：")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_and_previous_schema_keep_export_only_and_restore_notice() {
        let flags = [0, 0, 0, 0, 0, 1, 0];
        let previous = fixture(
            "2026-10-01T10:00:00Z",
            PREVIOUS_SCHEMA,
            "before_call_mode",
            flags,
        );
        let mut legacy = fixture("2026-10-01T10:00:00Z", 0, "", flags);
        legacy.as_object_mut().unwrap().remove("module");
        legacy.as_object_mut().unwrap().remove("voice");
        legacy["schema_version"] = Value::Null;
        legacy["kind"] = Value::Null;
        let previous = summarize(
            "usb-before-call-mode-v2.json",
            OffsetDateTime::UNIX_EPOCH,
            &serde_json::to_vec(&previous).unwrap(),
        )
        .0;
        let legacy = summarize(
            "usb-imported-legacy.json",
            OffsetDateTime::UNIX_EPOCH,
            &serde_json::to_vec(&legacy).unwrap(),
        )
        .0;
        assert!(previous.valid && previous.restorable);
        assert_eq!(
            previous.detail.as_deref(),
            Some("此备份只包含 USB 配置；还原时不会更改 IMS/VoLTE。")
        );
        assert!(legacy.valid && !legacy.restorable && legacy.imported);
        assert_eq!(
            legacy.detail.as_deref(),
            Some("旧版备份不包含模块身份，仅支持导出保存")
        );
    }

    #[test]
    fn invalid_schema_usb_and_voice_values_are_not_marked_restorable() {
        let good = [0, 0, 0, 0, 0, 0, 0];
        let cases = [
            (
                fixture("2026-10-01T10:00:00Z", 7, "before_call_mode", good),
                "不支持的备份格式版本 7",
            ),
            (
                fixture("2026-10-01T10:00:00Z", CURRENT_SCHEMA, "bad", good),
                "备份原因字段无效",
            ),
        ];
        for (backup, expected) in cases {
            let summary = summarize(
                "usb-before-call-mode-invalid.json",
                OffsetDateTime::UNIX_EPOCH,
                &serde_json::to_vec(&backup).unwrap(),
            )
            .0;
            assert!(!summary.valid && !summary.restorable);
            assert_eq!(summary.detail.as_deref(), Some(expected));
        }
        let mut bad_voice = fixture(
            "2026-10-01T10:00:00Z",
            CURRENT_SCHEMA,
            "before_call_mode",
            good,
        );
        bad_voice["voice"]["ims"] = json!(9);
        let summary = summarize(
            "usb-before-call-mode-invalid.json",
            OffsetDateTime::UNIX_EPOCH,
            &serde_json::to_vec(&bad_voice).unwrap(),
        )
        .0;
        assert_eq!(summary.detail.as_deref(), Some("IMS 配置值 9 无效"));
    }

    #[test]
    fn list_rejects_symlinks_and_oversized_files() {
        let root = test_root();
        let directory = backup_directory(&root);
        fs::create_dir_all(&directory).unwrap();
        let flags = [0, 0, 0, 0, 0, 0, 0];
        let bytes = serde_json::to_vec(&fixture(
            "2026-10-01T10:00:00Z",
            CURRENT_SCHEMA,
            "before_call_mode",
            flags,
        ))
        .unwrap();
        let target = directory.join("target.json");
        fs::write(&target, &bytes).unwrap();
        let large = directory.join("usb-before-restore-large.json");
        fs::write(&large, vec![b'x'; MAX_BACKUP_BYTES as usize + 1]).unwrap();
        let link = directory.join("usb-before-call-mode-link.json");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            list_backups(&root).unwrap().is_empty(),
            "backup hardlinks and symlinks are not listed"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn public_list_response_has_legacy_envelope() {
        let root = test_root();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/call-mode/backups")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = handle_public(request, &root).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({"backups":[]})
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn import_preserves_validated_json_and_private_file_modes() {
        use std::os::unix::fs::PermissionsExt;

        let root = test_root();
        let flags = [0, 0, 0, 0, 0, 1, 1];
        let mut payload = serde_json::to_vec_pretty(&fixture(
            "2026-10-01T10:00:00Z",
            CURRENT_SCHEMA,
            "before_call_mode",
            flags,
        ))
        .unwrap();
        payload.push(b'\n');

        let response = handle_public(import_request(Bytes::from(payload.clone())), &root).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let backup = &body["backup"];
        let id = backup["id"].as_str().unwrap();
        assert!(id.starts_with("usb-imported-"));
        assert!(valid_backup_id(id));
        assert_eq!(backup["file_name"], id);
        assert_eq!(backup["imported"], true);
        assert_eq!(backup["valid"], true);
        assert_eq!(backup["restorable"], true);
        assert_eq!(backup["module_imei"], "123456789012345");

        let (stored, _) = read_export(&root, id).unwrap();
        assert_eq!(
            stored, payload,
            "import must preserve the original JSON bytes"
        );
        let directory = backup_directory(&root);
        assert_eq!(
            fs::metadata(directory.join(id))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn rust_create_owner_preserves_go_filename_payload_and_private_modes() {
        use std::os::unix::fs::PermissionsExt;

        let root = test_root();
        let value = fixture(
            "2026-08-14T12:34:56.123456789+08:00",
            CURRENT_SCHEMA,
            "before_call_mode",
            [1, 1, 1, 1, 1, 0, 1],
        );
        let mut payload = serde_json::to_vec_pretty(&value).unwrap();
        payload.push(b'\n');
        let response = handle_internal(create_request(Bytes::from(payload.clone())), &root).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let expected = "usb-before-call-mode-20260814-123456.123456789-2345.json";
        assert_eq!(body["backup_id"], expected);
        let path = backup_directory(&root).join(expected);
        assert_eq!(fs::read(&path).unwrap(), payload);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(backup_directory(&root))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let backups = list_backups(&root).unwrap();
        assert_eq!(backups.len(), 1);
        assert!(backups[0].valid && backups[0].restorable);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn rust_create_owner_rejects_invalid_and_oversized_payloads_without_fallback() {
        let root = test_root();
        let mut invalid = fixture(
            "2026-08-14T12:34:56Z",
            CURRENT_SCHEMA,
            "before_call_mode",
            [1, 1, 1, 1, 1, 0, 1],
        );
        invalid["module"]["imei"] = json!("123");
        let response = handle_internal(
            create_request(Bytes::from(serde_json::to_vec(&invalid).unwrap())),
            &root,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!root.exists(), "invalid backup created a storage directory");

        let oversized = Bytes::from(vec![b'x'; MAX_BACKUP_BYTES as usize + 1]);
        let response = handle_internal(create_request(oversized), &root).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            !root.exists(),
            "oversized backup created a storage directory"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn internal_restore_data_returns_only_a_validated_restorable_backup() {
        let root = test_root();
        let id = "usb-before-call-mode-20261004-120000.000000000-2345.json";
        let payload = serde_json::to_vec(&fixture(
            "2026-10-04T12:00:00Z",
            CURRENT_SCHEMA,
            "before_call_mode",
            [1, 1, 1, 1, 1, 1, 1],
        ))
        .unwrap();
        fs::create_dir_all(backup_directory(&root)).unwrap();
        fs::write(backup_directory(&root).join(id), payload).unwrap();

        let request = restore_data_request(Bytes::from(
            serde_json::to_vec(&json!({"backup_id":id})).unwrap(),
        ));
        let response = handle_internal(request, &root).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["schema_version"], CURRENT_SCHEMA);
        assert_eq!(body["module"]["imei"], "123456789012345");
        assert_eq!(body["usb"]["flags"], json!([1, 1, 1, 1, 1, 1, 1]));
        assert_eq!(
            body["restore_command"],
            "AT+QCFG=\"USBCFG\",0x2C7C,0x2400,1,1,1,1,1,1,1"
        );

        let mut legacy = fixture(
            "2026-10-04T12:00:00Z",
            0,
            "before_call_mode",
            [1, 1, 1, 1, 1, 1, 1],
        );
        legacy["module"] = Value::Null;
        legacy["kind"] = Value::Null;
        legacy["reason"] = Value::Null;
        legacy["voice"] = Value::Null;
        let legacy_id = "usb-imported-20261004-120000.000000000-legacy.json";
        fs::write(
            backup_directory(&root).join(legacy_id),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let request = restore_data_request(Bytes::from(
            serde_json::to_vec(&json!({"backup_id":legacy_id})).unwrap(),
        ));
        let response = handle_internal(request, &root).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(body["error"].as_str().unwrap().contains("旧版备份"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn internal_restore_binding_uses_rust_backup_and_live_identity_facts() {
        let root = test_root();
        let id = "usb-before-call-mode-20261004-120000.000000000-2345.json";
        let payload = serde_json::to_vec(&fixture(
            "2026-10-04T12:00:00Z",
            CURRENT_SCHEMA,
            "before_call_mode",
            [1, 1, 1, 1, 1, 1, 1],
        ))
        .unwrap();
        fs::create_dir_all(backup_directory(&root)).unwrap();
        fs::write(backup_directory(&root).join(id), payload).unwrap();

        let valid = json!({
            "backup_id":id,
            "module_imei":"123456789012345",
            "vendor_id":11388,
            "product_id":9216
        });
        let response = handle_internal(
            validate_restore_request(Bytes::from(serde_json::to_vec(&valid).unwrap())),
            &root,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        for (imei, vendor, product, expected) in [
            (
                "123456789012346",
                11388,
                9216,
                "备份所属模块与当前连接模块的 IMEI 不一致",
            ),
            (
                "123456789012345",
                11389,
                9216,
                "备份与当前模块的 USB VID/PID 不一致",
            ),
            (
                "123456789012345",
                11388,
                9217,
                "备份与当前模块的 USB VID/PID 不一致",
            ),
        ] {
            let input = json!({
                "backup_id":id,
                "module_imei":imei,
                "vendor_id":vendor,
                "product_id":product
            });
            let response = handle_internal(
                validate_restore_request(Bytes::from(serde_json::to_vec(&input).unwrap())),
                &root,
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(body["error"], expected);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn internal_restore_data_rejects_tampered_restore_command() {
        let root = test_root();
        let id = "usb-before-call-mode-20261004-120001.000000000-2345.json";
        let mut tampered = fixture(
            "2026-10-04T12:00:00Z",
            CURRENT_SCHEMA,
            "before_call_mode",
            [1, 1, 1, 1, 1, 1, 1],
        );
        tampered["restore_command"] = json!("AT+QCFG=tampered");
        fs::create_dir_all(backup_directory(&root)).unwrap();
        fs::write(
            backup_directory(&root).join(id),
            serde_json::to_vec(&tampered).unwrap(),
        )
        .unwrap();

        let request = restore_data_request(Bytes::from(
            serde_json::to_vec(&json!({"backup_id":id})).unwrap(),
        ));
        let response = handle_internal(request, &root).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("还原命令与结构化 USB 配置不一致")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn import_rejects_tampering_empty_and_oversized_bodies_without_writing() {
        let root = test_root();
        let flags = [0, 0, 0, 0, 0, 1, 1];
        let mut tampered = fixture(
            "2026-10-01T10:00:00Z",
            CURRENT_SCHEMA,
            "before_call_mode",
            flags,
        );
        tampered["restore_command"] = json!("AT+QCFG=tampered");
        let tampered = serde_json::to_vec(&tampered).unwrap();
        for payload in [Bytes::from(tampered), Bytes::new()] {
            let response = handle_public(import_request(payload), &root).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }

        let response = handle_public(
            import_request(Bytes::from(vec![b'x'; MAX_BACKUP_BYTES as usize + 1])),
            &root,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .starts_with("读取导入备份失败：")
        );
        assert!(!backup_directory(&root).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn delete_preserves_confirmation_busy_order_and_removes_only_after_guard() {
        let root = test_root();
        let id = "usb-before-call-mode-delete.json";
        let directory = backup_directory(&root);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(id);
        fs::write(&path, b"{}").unwrap();
        let service = crate::test_module_radio(&root);

        let response = handle_delete_public(
            delete_request(delete_payload(false, id)),
            &root,
            service.clone(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(path.exists(), "unconfirmed delete removed backup");

        let guard = service.try_call_mode_backup_delete().unwrap();
        let missing = handle_delete_public(
            delete_request(delete_payload(true, "usb-before-call-mode-missing.json")),
            &root,
            service.clone(),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let busy = handle_delete_public(
            delete_request(delete_payload(true, id)),
            &root,
            service.clone(),
        )
        .await;
        assert_eq!(busy.status(), StatusCode::CONFLICT);
        assert!(path.exists(), "busy delete removed backup");
        drop(guard);

        let go_operation_lock = try_call_mode_operation_file_lock(&root).unwrap();
        let operation_busy = handle_delete_public(
            delete_request(delete_payload(true, id)),
            &root,
            service.clone(),
        )
        .await;
        assert_eq!(operation_busy.status(), StatusCode::CONFLICT);
        assert!(path.exists(), "delete ignored the Go operation file lock");
        drop(go_operation_lock);

        let deleted = handle_delete_public(
            delete_request(delete_payload(true, &format!(" {id} "))),
            &root,
            service,
        )
        .await;
        assert_eq!(deleted.status(), StatusCode::OK);
        let payload: Value =
            serde_json::from_slice(&deleted.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(payload, json!({"deleted":true,"backup_id":id}));
        assert!(!path.exists(), "confirmed delete kept backup");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn delete_rejects_unknown_fields_and_oversized_request_bodies() {
        let root = test_root();
        let service = crate::test_module_radio(&root);
        let unknown = handle_delete_public(
            delete_request(Bytes::from_static(
                br#"{"confirm":true,"backup_id":"usb-before-call-mode-x.json","extra":1}"#,
            )),
            &root,
            service.clone(),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

        let oversized = handle_delete_public(
            delete_request(Bytes::from(vec![b' '; MAX_DELETE_REQUEST_BYTES + 1])),
            &root,
            service,
        )
        .await;
        assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);
        let error: Value =
            serde_json::from_slice(&oversized.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(error["error"], "http: request body too large");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn import_uses_legacy_collision_suffix_and_keeps_legacy_export_only() {
        let root = test_root();
        let flags = [0, 0, 0, 0, 0, 1, 0];
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let current = serde_json::to_vec(&fixture(
            "2026-10-01T10:00:00Z",
            CURRENT_SCHEMA,
            "before_call_mode",
            flags,
        ))
        .unwrap();
        let first = write_imported_backup(&root, &current, now).unwrap();
        let second = write_imported_backup(&root, &current, now).unwrap();
        assert_eq!(
            first.file_name,
            imported_backup_id(&validate_import_payload(&current).unwrap(), now, 1)
        );
        assert!(second.file_name.ends_with("-2.json"));

        let mut legacy = fixture("2026-10-01T10:00:00Z", 0, "", flags);
        legacy.as_object_mut().unwrap().remove("module");
        legacy.as_object_mut().unwrap().remove("voice");
        legacy["kind"] = Value::Null;
        let legacy = serde_json::to_vec(&legacy).unwrap();
        let imported = write_imported_backup(&root, &legacy, now).unwrap();
        assert!(imported.imported && imported.valid && !imported.restorable);
        assert!(imported.id.ends_with("-legacy.json"));
        let _ = fs::remove_dir_all(root);
    }

    fn export_request(
        id: &str,
        range: Option<&str>,
        modified_since: Option<&str>,
    ) -> Request<Full<Bytes>> {
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(format!("/api/call-mode/backups/export?id={id}"));
        if let Some(range) = range {
            builder = builder.header(RANGE, range);
        }
        if let Some(modified_since) = modified_since {
            builder = builder.header(IF_MODIFIED_SINCE, modified_since);
        }
        builder.body(Full::new(Bytes::new())).unwrap()
    }

    async fn export_response_for(
        root: &Path,
        id: &str,
        range: Option<&str>,
        modified_since: Option<&str>,
    ) -> Response<Full<Bytes>> {
        handle_public(export_request(id, range, modified_since), root).await
    }

    #[tokio::test]
    async fn export_preserves_original_bytes_download_headers_and_conditional_get() {
        let root = test_root();
        let id = "usb-before-call-mode-export.json";
        let directory = backup_directory(&root);
        fs::create_dir_all(&directory).unwrap();
        let payload = b"{\n  \"original\": true\n}\n";
        let path = directory.join(id);
        fs::write(&path, payload).unwrap();
        let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_791_435_600);
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();

        let response = export_response_for(&root, id, None, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CONTENT_TYPE],
            "application/json; charset=utf-8"
        );
        assert_eq!(
            response.headers()[CONTENT_DISPOSITION],
            format!("attachment; filename=\"{id}\"")
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()[ACCEPT_RANGES], "bytes");
        assert_eq!(
            response.headers()[CONTENT_LENGTH],
            payload.len().to_string()
        );
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            payload
        );

        let response = export_response_for(
            &root,
            id,
            None,
            Some(&httpdate::fmt_http_date(
                modified + std::time::Duration::from_secs(1),
            )),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert!(!response.headers().contains_key(CONTENT_TYPE));
        assert!(!response.headers().contains_key(ACCEPT_RANGES));
        assert!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn export_dates_before_epoch_and_go_zero_sentinels_are_panic_free() {
        let before_epoch = SystemTime::UNIX_EPOCH
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap();
        let date = "Wed, 31 Dec 1969 23:59:59 GMT";
        assert_eq!(export_http_date(before_epoch).as_deref(), Some(date));
        for spelling in [
            date,
            "Wednesday, 31-Dec-69 23:59:59 GMT",
            "Wed Dec 31 23:59:59 1969",
            "Wed, 31 Dec 1969 23:59:59.123 GMT",
        ] {
            assert_eq!(
                parse_export_http_date(spelling),
                Some(before_epoch),
                "{spelling}"
            );
        }
        let mut headers = HeaderMap::new();
        headers.insert(RANGE, HeaderValue::from_static("bytes=0-1"));
        headers.insert(IF_RANGE, HeaderValue::from_static(date));
        let response = export_response(
            "usb-before-call-mode-old.json",
            b"0123456789".to_vec(),
            before_epoch,
            &headers,
        );
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[LAST_MODIFIED], date);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "01"
        );
        headers.insert(IF_MODIFIED_SINCE, HeaderValue::from_static(date));
        assert_eq!(
            export_response(
                "usb-before-call-mode-old.json",
                b"0123456789".to_vec(),
                before_epoch,
                &headers
            )
            .status(),
            StatusCode::NOT_MODIFIED
        );

        // isZeroTime (Last-Modified/IMS) includes Unix epoch, but Go's
        // checkIfRange uses Time.IsZero and only excludes exact year1 zero.
        headers.insert(
            IF_RANGE,
            HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT"),
        );
        let response = export_response(
            "usb-before-call-mode-zero.json",
            b"0123456789".to_vec(),
            SystemTime::UNIX_EPOCH,
            &headers,
        );
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert!(!response.headers().contains_key(LAST_MODIFIED));
        let year_one = SystemTime::UNIX_EPOCH
            .checked_sub(std::time::Duration::from_secs(62_135_596_800))
            .unwrap();
        headers.insert(
            IF_MODIFIED_SINCE,
            HeaderValue::from_static("Mon, 01 Jan 0001 00:00:00 GMT"),
        );
        headers.insert(
            IF_RANGE,
            HeaderValue::from_static("Mon, 01 Jan 0001 00:00:00 GMT"),
        );
        let response = export_response(
            "usb-before-call-mode-zero.json",
            b"0123456789".to_vec(),
            year_one,
            &headers,
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key(LAST_MODIFIED));

        let unsupported = SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(253_402_300_800))
            .unwrap();
        let response = export_response(
            "usb-before-call-mode-extreme.json",
            b"0123456789".to_vec(),
            unsupported,
            &HeaderMap::new(),
        );
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let error: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(error["error"], "读取备份失败：备份文件修改时间超出支持范围");
    }

    #[tokio::test]
    async fn export_matches_go_precondition_precedence_and_if_range_date_equality() {
        // Historical callModeBackupExportAPI delegates to Go ServeContent.
        // These expectations follow retained Go1.26.7 checkPreconditions.
        let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_791_435_600);
        let date = "Thu, 08 Oct 2026 05:00:00 GMT";
        let older = "Thu, 01 Jan 1970 00:00:00 GMT";
        let future = "Thu, 08 Oct 2026 05:00:01 GMT";
        for (fields, status, bytes) in [
            (
                vec![("if-match", "\"absent\""), ("range", "bytes=0-1")],
                412,
                "",
            ),
            (vec![("if-unmodified-since", older)], 412, ""),
            (
                vec![("if-match", "*"), ("if-unmodified-since", older)],
                200,
                "0123456789",
            ),
            (
                vec![("if-none-match", "*"), ("range", "bytes=0-1")],
                304,
                "",
            ),
            (
                vec![("if-none-match", "\"absent\""), ("if-modified-since", date)],
                200,
                "0123456789",
            ),
            (vec![("if-none-match", "W/\"absent\", *")], 304, ""),
            (
                vec![
                    ("if-none-match", "\"invalid tag\", *"),
                    ("if-modified-since", date),
                ],
                200,
                "0123456789",
            ),
            (
                vec![("if-range", future), ("range", "bytes=0-1")],
                200,
                "0123456789",
            ),
            (vec![("if-range", date), ("range", "bytes=0-1")], 206, "01"),
            (
                vec![("if-range", "W/\"absent\""), ("range", "bytes=0-1")],
                200,
                "0123456789",
            ),
        ] {
            let mut headers = HeaderMap::new();
            for (name, value) in &fields {
                headers.insert(
                    hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    HeaderValue::from_str(value).unwrap(),
                );
            }
            let response = export_response(
                "usb-before-call-mode-fixture.json",
                b"0123456789".to_vec(),
                modified,
                &headers,
            );
            assert_eq!(response.status().as_u16(), status, "{fields:?}");
            if status == 304 {
                assert!(!response.headers().contains_key(CONTENT_TYPE));
                assert!(!response.headers().contains_key(CONTENT_LENGTH));
            }
            if status == 304 || status == 412 {
                assert!(!response.headers().contains_key(ACCEPT_RANGES));
                assert_eq!(response.headers()[LAST_MODIFIED], date);
            }
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                bytes,
                "{fields:?}"
            );
        }
    }

    #[tokio::test]
    async fn export_matches_go_range_zero_suffix_signed_limit_and_amplification_fallback() {
        let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_791_435_600);
        for (range, status, content_range, bytes) in [
            ("bytes=-0", 206, Some("bytes 10-9/10"), ""),
            ("bytes=0-9,0-9", 200, None, "0123456789"),
            ("bytes=0-9,0-0", 200, None, "0123456789"),
            ("bytes=-9223372036854775808", 416, None, "invalid range\n"),
            ("bytes=9223372036854775808-", 416, None, "invalid range\n"),
            (
                "bytes=99-100",
                416,
                Some("bytes */10"),
                "invalid range: failed to overlap\n",
            ),
            ("bytes=99-100,0-1", 206, Some("bytes 0-1/10"), "01"),
            ("bytes=,", 200, None, "0123456789"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(RANGE, HeaderValue::from_str(range).unwrap());
            let response = export_response(
                "usb-before-call-mode-fixture.json",
                b"0123456789".to_vec(),
                modified,
                &headers,
            );
            assert_eq!(response.status().as_u16(), status, "{range}");
            assert_eq!(
                response
                    .headers()
                    .get(CONTENT_RANGE)
                    .map(|value| value.to_str().unwrap()),
                content_range,
                "{range}"
            );
            if status == 416 {
                assert!(!response.headers().contains_key(LAST_MODIFIED));
                assert!(!response.headers().contains_key(ACCEPT_RANGES));
            }
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                bytes,
                "{range}"
            );
        }
    }

    #[tokio::test]
    async fn export_supports_single_suffix_and_multipart_byte_ranges() {
        let root = test_root();
        let id = "usb-before-call-mode-ranges.json";
        let directory = backup_directory(&root);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(id), b"0123456789").unwrap();

        let response = export_response_for(&root, id, Some("bytes=2-5"), None).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "2345"
        );

        let response = export_response_for(&root, id, Some("bytes=-3"), None).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "789"
        );

        let response = export_response_for(&root, id, Some("bytes=0-1,8-9"), None).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert!(
            response.headers()[CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("multipart/byteranges; boundary=")
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.windows(2).any(|window| window == b"01"));
        assert!(body.windows(2).any(|window| window == b"89"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn export_errors_keep_not_found_envelope_and_reject_symlinks_and_bad_ranges() {
        let root = test_root();
        let id = "usb-before-call-mode-valid.json";
        let directory = backup_directory(&root);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(id), b"0123456789").unwrap();

        let response = export_response_for(&root, "../outside.json", None, None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let error: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(error["error"], "读取备份失败：备份文件标识无效");

        let response = export_response_for(&root, id, Some("bytes=99-100"), None).await;
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers()[CONTENT_RANGE], "bytes */10");
        assert_eq!(
            response.headers()[CONTENT_DISPOSITION],
            format!("attachment; filename=\"{id}\"")
        );
        assert!(!response.headers().contains_key(ACCEPT_RANGES));
        assert!(!response.headers().contains_key(LAST_MODIFIED));
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        let invalid_range = export_response_for(&root, id, Some("items=0-1"), None).await;
        assert_eq!(invalid_range.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert!(!invalid_range.headers().contains_key(CONTENT_RANGE));
        assert!(!invalid_range.headers().contains_key(ACCEPT_RANGES));
        assert!(!invalid_range.headers().contains_key(LAST_MODIFIED));
        assert_eq!(
            invalid_range
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes(),
            "invalid range\n"
        );

        // The historical application rejects empty files before ServeContent;
        // its special empty-content Range=200 branch is unreachable here.
        let empty_id = "usb-before-call-mode-empty.json";
        fs::write(directory.join(empty_id), b"").unwrap();
        let empty = export_response_for(&root, empty_id, Some("bytes=0-"), None).await;
        assert_eq!(empty.status(), StatusCode::NOT_FOUND);
        let error: Value =
            serde_json::from_slice(&empty.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(error["error"], "读取备份失败：备份文件类型或大小无效");

        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("/api/call-mode/backups/export?id={id}"))
            .header(RANGE, "bytes=0-1")
            .header(IF_RANGE, httpdate::fmt_http_date(SystemTime::UNIX_EPOCH))
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = handle_public(request, &root).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "0123456789"
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                directory.join(id),
                directory.join("usb-before-call-mode-link.json"),
            )
            .unwrap();
            let response =
                export_response_for(&root, "usb-before-call-mode-link.json", None, None).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            let error: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(error["error"], "读取备份失败：备份文件类型或大小无效");
        }
        let _ = fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod native_tests;

#[cfg(test)]
pub(crate) mod lifecycle_tests;
