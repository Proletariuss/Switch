//! Rust-owned persistence and public API for device SMS adoption mode.
//!
//! The on-disk shape remains `{"enabled": bool}`, preserving the existing
//! Switch data file while moving its writer and runtime owner out of Go.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::body::Body;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;

const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 1 << 20;

#[derive(Clone)]
pub(crate) struct Store {
    path: PathBuf,
}

#[derive(Debug)]
pub(crate) enum StoreError {
    Unavailable,
    Worker,
}

#[derive(Deserialize, Serialize)]
struct Persisted {
    enabled: bool,
}

impl Store {
    pub(crate) fn new(data_root: &Path) -> Self {
        Self {
            path: data_root.join("sms-adopt.json"),
        }
    }

    pub(crate) fn load(&self) -> Result<bool, StoreError> {
        ensure_regular_or_missing(&self.path)?;
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(StoreError::Unavailable),
        };
        let size = file.metadata().map_err(|_| StoreError::Unavailable)?.len();
        if size > MAX_FILE_BYTES {
            return Err(StoreError::Unavailable);
        }
        let mut bytes = Vec::with_capacity(size as usize);
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| StoreError::Unavailable)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(StoreError::Unavailable);
        }
        let persisted: Persisted =
            serde_json::from_slice(&bytes).map_err(|_| StoreError::Unavailable)?;
        Ok(persisted.enabled)
    }

    pub(crate) fn save(&self, enabled: bool) -> Result<(), StoreError> {
        let parent = self.path.parent().ok_or(StoreError::Unavailable)?;
        fs::create_dir_all(parent).map_err(|_| StoreError::Unavailable)?;
        if fs::symlink_metadata(parent)
            .map(|metadata| !metadata.is_dir())
            .unwrap_or(true)
        {
            return Err(StoreError::Unavailable);
        }
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|_| StoreError::Unavailable)?;
        ensure_regular_or_missing(&self.path)?;
        let lock_path = self.path.with_extension("json.lock");
        ensure_regular_or_missing(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|_| StoreError::Unavailable)?;
        lock.lock().map_err(|_| StoreError::Unavailable)?;

        let file_name = self
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(StoreError::Unavailable)?;
        let temporary = parent.join(format!(
            ".{file_name}.{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| StoreError::Unavailable)?;
        let result = (|| {
            let bytes =
                serde_json::to_vec(&Persisted { enabled }).map_err(|_| StoreError::Unavailable)?;
            file.write_all(&bytes)
                .map_err(|_| StoreError::Unavailable)?;
            file.sync_all().map_err(|_| StoreError::Unavailable)?;
            ensure_regular_or_missing(&self.path)?;
            fs::rename(&temporary, &self.path).map_err(|_| StoreError::Unavailable)?;
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| StoreError::Unavailable)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

fn ensure_regular_or_missing(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(StoreError::Unavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(StoreError::Unavailable),
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SetRequest {
    enabled: Option<bool>,
}

pub(crate) async fn handle_public<B>(
    request: Request<B>,
    owner: crate::usb_at::Owner,
    store: Store,
) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/sms/adopt") => match owner.adoption_enabled().await {
            Ok(enabled) => crate::json_response(StatusCode::OK, &json!({"enabled":enabled})),
            Err(()) => json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error":"短信接管状态暂不可用","code":"sms_adoption_unavailable","retryable":true}),
            ),
        },
        (&Method::POST, "/api/sms/adopt") => {
            let enabled = match decode_request(request).await {
                Ok(enabled) => enabled,
                Err(response) => return response,
            };
            match owner.set_adoption(store, enabled).await {
                Ok(()) => crate::json_response(StatusCode::OK, &json!({"adopt":enabled})),
                Err(_) => json_response(
                    StatusCode::BAD_GATEWAY,
                    &json!({"error":"短信接管设置同步未确认，请重试","code":"usb_sms_configuration_unconfirmed","retryable":true}),
                ),
            }
        }
        _ => json_response(StatusCode::NOT_FOUND, &json!({"error":"route not found"})),
    }
}

async fn decode_request<B>(request: Request<B>) -> Result<bool, Response<Full<Bytes>>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let bytes = match crate::http_server::collect_body(request.into_body(), MAX_REQUEST_BYTES).await
    {
        Ok(body) => body,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return Err(crate::http_server::body_timeout_response());
        }
        Err(crate::http_server::BodyReadError::TooLarge) => {
            return Err(invalid_request("http: request body too large"));
        }
        Err(_) => {
            return Err(invalid_request("读取请求失败"));
        }
    };
    let mut decoder = serde_json::Deserializer::from_slice(&bytes);
    let request: Option<SetRequest> =
        Option::deserialize(&mut decoder).map_err(|error| invalid_request(error.to_string()))?;
    decoder
        .end()
        .map_err(|_| invalid_request("请求只能包含一个 JSON 对象"))?;
    Ok(request.and_then(|request| request.enabled).unwrap_or(false))
}

fn invalid_request(message: impl Into<String>) -> Response<Full<Bytes>> {
    json_response(StatusCode::BAD_REQUEST, &json!({"error":message.into()}))
}

fn json_response(status: StatusCode, value: &serde_json::Value) -> Response<Full<Bytes>> {
    crate::json_response(status, value)
}

#[cfg(test)]
#[path = "sms_adopt_tests.rs"]
mod tests;
