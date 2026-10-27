//! Detached, bounded and hash-pinned voice-runtime downloads. The HTTP request
//! only starts/observes a job; its lifetime never owns the download worker.

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use nix::fcntl::{AtFlags, Flock, FlockArg, OFlag};
use nix::sys::stat::Mode;
use serde::{Deserialize, Serialize};

use crate::call_mode_runtime::verify_artifact_bytes;
use crate::call_mode_runtime_manifest::{Artifact, RuntimeManifest, manifest};

pub(crate) const PUBLIC_DOWNLOAD_PATH: &str = "/api/call-mode/download";
const PUBLIC_REQUEST_LIMIT: usize = 1024 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(120);
const DOWNLOAD_OBSERVATION: Duration = Duration::from_secs(26 * 60);
static ADMISSION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    source: String,
    operation_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicStartRequest {
    confirm: bool,
    source: String,
}

impl PublicStartRequest {
    fn valid(&self) -> bool {
        self.confirm && matches!(self.source.as_str(), "github" | "relay")
    }
}

impl StartRequest {
    fn valid(&self) -> bool {
        matches!(self.source.as_str(), "github" | "relay")
            && self.operation_id.len() == 32
            && self
                .operation_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

pub(crate) async fn handle_public(
    request: Request<Incoming>,
    service: crate::call_mode_lifecycle::Service,
) -> Response<Full<Bytes>> {
    if request.uri().path() != PUBLIC_DOWNLOAD_PATH || request.uri().query().is_some() {
        return legacy_failure(StatusCode::NOT_FOUND, "route not found");
    }
    if request.method() != Method::POST {
        return legacy_failure(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }
    let body = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        PUBLIC_REQUEST_LIMIT,
        Duration::from_secs(3),
    )
    .await
    {
        Ok(body) => body,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return legacy_failure(StatusCode::BAD_REQUEST, "请求体超过大小限制"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return legacy_failure(StatusCode::REQUEST_TIMEOUT, "请求读取超时");
        }
    };
    let input = match serde_json::from_slice::<PublicStartRequest>(&body) {
        Ok(input) => input,
        Err(error) => return legacy_failure(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    if !input.confirm {
        return legacy_failure(StatusCode::BAD_REQUEST, "需要确认后才会下载第三方运行时");
    }
    if !matches!(input.source.as_str(), "github" | "relay") {
        return legacy_failure(StatusCode::BAD_REQUEST, "下载来源必须是 github 或 relay");
    }
    debug_assert!(input.valid());
    service.start_download(input.source).await
}

/// A native download reports only facts from its real, lock-owning worker.
#[derive(Clone, Debug)]
pub(crate) struct DownloadProgress {
    pub(crate) downloaded_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) detail: String,
}

/// Runs through completion and keeps admission plus the process file lock in
/// the actual blocking worker. Cancellation is cooperative between reads and
/// before publishing files; callers must await this worker before releasing
/// their radio reservation or starting deployment.
pub(crate) async fn run_native(
    data_root: &Path,
    recovery: crate::module_recovery::Gate,
    operation_id: String,
    source: String,
    current: Arc<dyn Fn() -> bool + Send + Sync>,
    progress: Arc<dyn Fn(DownloadProgress) + Send + Sync>,
) -> Result<(), String> {
    let input = StartRequest {
        source,
        operation_id,
    };
    if !input.valid() {
        return Err("需要合法下载来源和 32 位小写十六进制操作编号".into());
    }
    let permit = ADMISSION
        .try_acquire()
        .map_err(|_| "正在受理其他运行时下载请求".to_owned())?;
    let root = data_root.to_owned();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let started = Instant::now();
        let active = || current() && started.elapsed() < DOWNLOAD_OBSERVATION
            && recovery.root_proof_for(&root).is_ok();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ensure_current(&active)?;
            let directory = match crate::module_recovery::runtime_download::begin(&root, &recovery)
                .map_err(|error| error.to_string())? {
                Some(controlled) => RuntimeDirectory {
                    parent: controlled.runtime,
                    staging: Some(controlled.staging),
                },
                None => RuntimeDirectory::open(&root)?,
            };
            let _process_lock = directory.try_lock()?;
            let transport = HttpsTransport::new()?;
            let runtime = manifest();
            let total = runtime.files.iter().map(|file| file.size).sum();
            let mut highest = 0;
            download_runtime_fenced(
                &directory,
                runtime,
                &input,
                &transport,
                |_, count, detail| {
                    // A failed relay attempt must not regress the public progress.
                    highest = highest.max(count);
                    if active() {
                        progress(DownloadProgress {
                            downloaded_bytes: highest,
                            total_bytes: total,
                            detail: detail.into(),
                        });
                    }
                },
                &active,
            )?;
            ensure_current(&active)
        }))
        .unwrap_or_else(|_| Err("运行时下载工作线程异常退出".into()));
        if started.elapsed() >= DOWNLOAD_OBSERVATION {
            Err("下载观察超过 26 分钟，未确认运行时完整性".into())
        } else {
            result
        }
    })
    .await
    .map_err(|_| "运行时下载工作线程异常退出".to_owned())?
}

fn ensure_current(current: &dyn Fn() -> bool) -> Result<(), String> {
    if current() {
        Ok(())
    } else {
        Err("运行时下载操作已失效".into())
    }
}

fn legacy_failure(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    json(status, &serde_json::json!({"error": message}))
}

pub(crate) fn new_operation_id() -> Result<String, ()> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

struct DownloadResponse {
    status: u16,
    final_url: String,
    content_length: Option<u64>,
    body: Box<dyn Read + Send>,
}

trait Transport: Send {
    fn get(&self, url: &str) -> Result<DownloadResponse, String>;
}

struct HttpsTransport(reqwest::blocking::Client);

impl HttpsTransport {
    fn new() -> Result<Self, String> {
        reqwest::blocking::Client::builder()
            .timeout(ATTEMPT_TIMEOUT)
            .user_agent("Proletarius/voice-runtime-downloader")
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    return attempt.error("下载重定向次数过多");
                }
                if attempt.url().scheme() != "https" {
                    return attempt.error("运行时下载不允许跳转到非 HTTPS 地址");
                }
                attempt.follow()
            }))
            .build()
            .map(Self)
            .map_err(|error| format!("无法创建下载客户端：{error}"))
    }
}

impl Transport for HttpsTransport {
    fn get(&self, url: &str) -> Result<DownloadResponse, String> {
        require_https(url)?;
        let response = self
            .0
            .get(url)
            .send()
            .map_err(|error| format!("运行时下载请求失败：{error}"))?;
        Ok(DownloadResponse {
            status: response.status().as_u16(),
            final_url: response.url().as_str().into(),
            content_length: response.content_length(),
            body: Box::new(response),
        })
    }
}

fn require_https(url: &str) -> Result<(), String> {
    match url::Url::parse(url) {
        Ok(url)
            if url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none() =>
        {
            Ok(())
        }
        _ => Err("运行时下载地址必须是 HTTPS".into()),
    }
}

fn urls(runtime: &RuntimeManifest, source: &str, file: &Artifact) -> Vec<String> {
    let original = format!(
        "https://raw.githubusercontent.com/moluncn/mavo/{}/Resources/ModuleVoice/{}",
        runtime.commit, file.name
    );
    if source == "relay" {
        vec![
            format!("https://ghfast.top/{original}"),
            format!("https://gh-proxy.com/{original}"),
        ]
    } else {
        vec![original]
    }
}

#[cfg(test)]
fn download_runtime<F>(
    directory: &RuntimeDirectory,
    runtime: &RuntimeManifest,
    input: &StartRequest,
    transport: &dyn Transport,
    progress: F,
) -> Result<(), String>
where
    F: FnMut(&str, u64, &str),
{
    download_runtime_fenced(directory, runtime, input, transport, progress, &|| true)
}

fn download_runtime_fenced<F>(
    directory: &RuntimeDirectory,
    runtime: &RuntimeManifest,
    input: &StartRequest,
    transport: &dyn Transport,
    mut progress: F,
    current: &dyn Fn() -> bool,
) -> Result<(), String>
where
    F: FnMut(&str, u64, &str),
{
    ensure_current(current)?;
    let destination = directory.open_version(&runtime.version)?;
    let mut completed = 0;
    for artifact in &runtime.files {
        ensure_current(current)?;
        ensure_destination_regular(&destination, artifact)?;
        if read_cached(&destination, artifact).is_err() {
            let mut downloaded = None;
            let mut failures = Vec::new();
            for url in urls(runtime, &input.source, artifact) {
                ensure_current(current)?;
                match download_file_fenced(
                    transport,
                    &url,
                    artifact,
                    |received| {
                        progress(
                            &artifact.name,
                            completed + received,
                            &format!("正在下载 {}", artifact.name),
                        );
                    },
                    current,
                ) {
                    Ok(result) => {
                        downloaded = Some(result);
                        break;
                    }
                    Err(error) => failures.push(error),
                }
            }
            let bytes = downloaded.ok_or_else(|| failures.join("；"))?;
            ensure_current(current)?;
            install_file_staged(&destination, directory.staging.as_ref(), artifact, &bytes, &input.operation_id, current)?;
        }
        completed += artifact.size;
        progress(
            &artifact.name,
            completed,
            &format!("已完成 {}", artifact.name),
        );
    }
    // Reopen the published directory through its anchored parent, so replacing
    // the path during a download cannot produce a false successful completion.
    ensure_current(current)?;
    let installed = open_at(
        &directory.parent,
        &runtime.version,
        nix::libc::O_RDONLY | nix::libc::O_DIRECTORY,
        0,
    )
    .map_err(|_| "运行时安装目录在下载期间被替换".to_owned())?;
    for artifact in &runtime.files {
        ensure_current(current)?;
        read_cached(&installed, artifact)?;
    }
    destination
        .sync_all()
        .map_err(|error| format!("同步运行时目录失败：{error}"))?;
    Ok(())
}

#[cfg(test)]
fn download_file<F>(
    transport: &dyn Transport,
    url: &str,
    artifact: &Artifact,
    progress: F,
) -> Result<Vec<u8>, String>
where
    F: FnMut(u64),
{
    download_file_fenced(transport, url, artifact, progress, &|| true)
}

fn download_file_fenced<F>(
    transport: &dyn Transport,
    url: &str,
    artifact: &Artifact,
    mut progress: F,
    current: &dyn Fn() -> bool,
) -> Result<Vec<u8>, String>
where
    F: FnMut(u64),
{
    ensure_current(current)?;
    require_https(url)?;
    let mut response = transport.get(url)?;
    ensure_current(current)?;
    require_https(&response.final_url)?;
    if response.status != 200 {
        return Err(format!("{}：HTTP {}", artifact.name, response.status));
    }
    if response
        .content_length
        .is_some_and(|length| length != artifact.size)
    {
        return Err(format!("{}：服务器返回大小不匹配", artifact.name));
    }
    let mut bytes = Vec::with_capacity(artifact.size as usize);
    let mut limited = (&mut response.body).take(artifact.size + 1);
    let mut buffer = [0u8; 16 * 1024];
    let mut last = None::<Instant>;
    loop {
        ensure_current(current)?;
        let count = limited
            .read(&mut buffer)
            .map_err(|error| format!("下载 {} 失败：{error}", artifact.name))?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if last.is_none_or(|last| last.elapsed() >= PROGRESS_INTERVAL) {
            progress((bytes.len() as u64).min(artifact.size));
            last = Some(Instant::now());
        }
    }
    ensure_current(current)?;
    verify_artifact_bytes(artifact, &bytes)?;
    Ok(bytes)
}

fn name(value: &str) -> Result<CString, String> {
    if value.is_empty() || value == "." || value == ".." || value.contains('/') {
        return Err("运行时文件名无效".into());
    }
    CString::new(value).map_err(|_| "运行时文件名无效".into())
}

fn open_at(parent: &File, entry: &str, flags: i32, mode: u32) -> Result<File, std::io::Error> {
    let entry = name(entry).map_err(std::io::Error::other)?;
    nix::fcntl::openat(
        parent,
        entry.as_c_str(),
        OFlag::from_bits_truncate(flags) | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(mode as nix::libc::mode_t),
    )
    .map(File::from)
    .map_err(std::io::Error::from)
}

struct RuntimeDirectory {
    parent: File,
    staging: Option<File>,
}

impl RuntimeDirectory {
    fn open(root: &Path) -> Result<Self, String> {
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY)
            .open(root)
            .map_err(|error| format!("Switch 数据目录不可用：{error}"))?;
        let component = name("voice-runtime")?;
        if let Err(error) = nix::sys::stat::mkdirat(&root, component.as_c_str(), Mode::S_IRWXU) {
            if error != nix::errno::Errno::EEXIST {
                return Err("无法创建运行时父目录".into());
            }
        }
        let parent = open_at(
            &root,
            "voice-runtime",
            nix::libc::O_RDONLY | nix::libc::O_DIRECTORY,
            0,
        )
        .map_err(|error| format!("运行时父目录不可用：{error}"))?;
        root.sync_all()
            .map_err(|error| format!("同步运行时父目录入口失败：{error}"))?;
        Ok(Self { parent, staging: None })
    }

    fn try_lock(&self) -> Result<Flock<File>, String> {
        let lock = open_at(
            &self.parent,
            ".download.lock",
            nix::libc::O_RDWR | nix::libc::O_CREAT | nix::libc::O_NONBLOCK,
            0o600,
        )
        .map_err(|error| format!("无法打开下载锁：{error}"))?;
        let meta = lock.metadata().map_err(|_| "无法读取下载锁状态")?;
        if !meta.is_file() || meta.nlink() != 1 {
            return Err("下载锁必须是独立普通文件".into());
        }
        Flock::lock(lock, FlockArg::LockExclusiveNonblock)
            .map_err(|_| "其他进程正在下载语音运行时".into())
    }

    fn open_version(&self, version: &str) -> Result<File, String> {
        let component = name(version)?;
        if let Err(error) =
            nix::sys::stat::mkdirat(&self.parent, component.as_c_str(), Mode::S_IRWXU)
        {
            if error != nix::errno::Errno::EEXIST {
                return Err("无法创建运行时目录".into());
            }
        }
        let directory = open_at(
            &self.parent,
            version,
            nix::libc::O_RDONLY | nix::libc::O_DIRECTORY,
            0,
        )
        .map_err(|error| format!("运行时目录不可用：{error}"))?;
        self.parent
            .sync_all()
            .map_err(|error| format!("同步运行时父目录失败：{error}"))?;
        Ok(directory)
    }
}

fn ensure_destination_regular(directory: &File, artifact: &Artifact) -> Result<(), String> {
    let entry = name(&artifact.name)?;
    let metadata =
        match nix::sys::stat::fstatat(directory, entry.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(metadata) => metadata,
            Err(nix::errno::Errno::ENOENT) => return Ok(()),
            Err(_) => return Err(format!("无法检查 {} 的文件类型", artifact.name)),
        };
    if metadata.st_mode & nix::libc::S_IFMT != nix::libc::S_IFREG {
        return Err(format!("{} 不是普通文件，已拒绝覆盖", artifact.name));
    }
    Ok(())
}

#[cfg(test)]
fn install_file(
    directory: &File,
    artifact: &Artifact,
    bytes: &[u8],
    operation: &str,
) -> Result<(), String> {
    install_file_fenced(directory, artifact, bytes, operation, &|| true)
}
#[cfg(test)]
fn install_file_fenced(
    directory: &File,
    artifact: &Artifact,
    bytes: &[u8],
    operation: &str,
    current: &dyn Fn() -> bool,
) -> Result<(), String> {
    install_file_staged(directory, None, artifact, bytes, operation, current)
}
fn install_file_staged(
    directory: &File,
    staging: Option<&File>,
    artifact: &Artifact,
    bytes: &[u8],
    operation: &str,
    current: &dyn Fn() -> bool,
) -> Result<(), String> {
    ensure_current(current)?;
    verify_artifact_bytes(artifact, bytes)?;
    let staging = staging.unwrap_or(directory);
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|_| "无法创建运行时临时文件")?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let entry = format!(".{}-{operation}-{suffix}.part", artifact.name);
    let mut file = open_at(
        staging,
        &entry,
        nix::libc::O_WRONLY | nix::libc::O_CREAT | nix::libc::O_EXCL,
        0o600,
    )
    .map_err(|error| format!("无法暂存 {}：{error}", artifact.name))?;
    let _cleanup = TemporaryFile {
        directory: staging,
        entry: &entry,
    };
    file.write_all(bytes)
        .map_err(|error| format!("保存 {} 失败：{error}", artifact.name))?;
    file.set_permissions(std::fs::Permissions::from_mode(artifact.mode))
        .map_err(|error| format!("设置 {} 权限失败：{error}", artifact.name))?;
    file.sync_all()
        .map_err(|error| format!("同步 {} 失败：{error}", artifact.name))?;
    ensure_destination_regular(directory, artifact)?;
    let from = name(&entry)?;
    let to = name(&artifact.name)?;
    ensure_current(current)?;
    nix::fcntl::renameat(staging, from.as_c_str(), directory, to.as_c_str())
        .map_err(|error| format!("提交 {} 失败：{error}", artifact.name))?;
    staging.sync_all().map_err(|error| format!("同步下载暂存目录失败：{error}"))?;
    directory
        .sync_all()
        .map_err(|error| format!("同步 {} 安装结果失败：{error}", artifact.name))?;
    Ok(())
}

struct TemporaryFile<'a> {
    directory: &'a File,
    entry: &'a str,
}

impl Drop for TemporaryFile<'_> {
    fn drop(&mut self) {
        unlink(self.directory, self.entry, false);
    }
}

fn unlink(parent: &File, entry: &str, directory: bool) {
    if let Ok(entry) = name(entry) {
        let _ = nix::unistd::unlinkat(
            parent,
            entry.as_c_str(),
            if directory {
                nix::unistd::UnlinkatFlags::RemoveDir
            } else {
                nix::unistd::UnlinkatFlags::NoRemoveDir
            },
        );
    }
}

fn read_cached(directory: &File, artifact: &Artifact) -> Result<Vec<u8>, String> {
    let file = open_at(
        directory,
        &artifact.name,
        nix::libc::O_RDONLY | nix::libc::O_NONBLOCK,
        0,
    )
    .map_err(|_| format!("{} 缓存不可用", artifact.name))?;
    let meta = file
        .metadata()
        .map_err(|_| format!("{} 缓存状态不可用", artifact.name))?;
    if !meta.is_file() || meta.len() != artifact.size {
        return Err(format!("{} 缓存类型或大小不匹配", artifact.name));
    }
    let mut bytes = Vec::with_capacity(artifact.size as usize);
    file.take(artifact.size + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| format!("{} 缓存读取失败", artifact.name))?;
    verify_artifact_bytes(artifact, &bytes)?;
    Ok(bytes)
}

fn json<T: Serialize>(status: StatusCode, value: &T) -> Response<Full<Bytes>> {
    crate::json_response(status, value)
}

#[cfg(test)]
mod tests;
