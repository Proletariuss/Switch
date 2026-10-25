//! Compatibility implementation of the retired module UAC API.
//!
//! The GET endpoint reads the actual module setting through the Rust modem
//! actor and remembers the last valid value in the existing JSON file. The
//! POST endpoint stays disabled exactly as before; changing USB composition is
//! only supported through the confirmed call-mode workflow.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Body;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_STATE_BYTES: u64 = 4096;
const DISABLED_MESSAGE: &str = "此旧接口已停用，请在通话页面完成带确认和备份的通话模式初始化";

#[derive(Clone)]
struct Store {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct StateFile {
    #[serde(default)]
    enabled: bool,
}

impl Store {
    fn new(data_root: &Path) -> Self {
        Self {
            path: data_root.join("voice-enabled.json"),
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn load(&self) -> bool {
        self.with_lock(|_| self.load_unlocked()).unwrap_or(false)
    }

    fn save_best_effort(&self, enabled: bool) {
        let _ = self.with_lock(|_| {
            if self.load_unlocked()? == enabled {
                return Ok(());
            }
            self.persist_unlocked(enabled)
        });
    }

    fn with_lock<T>(
        &self,
        operation: impl FnOnce(&File) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let _local = self.lock.lock().map_err(|_| StoreError)?;
        let parent = self.path.parent().ok_or(StoreError)?;
        fs::create_dir_all(parent).map_err(|_| StoreError)?;
        if fs::symlink_metadata(parent)
            .map(|metadata| !metadata.is_dir())
            .unwrap_or(true)
        {
            return Err(StoreError);
        }
        ensure_regular_or_missing(&self.path)?;
        let lock_path = self.path.with_extension("json.lock");
        ensure_regular_or_missing(&lock_path)?;
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)
            .map_err(|_| StoreError)?;
        lock_file.lock().map_err(|_| StoreError)?;
        operation(&lock_file)
    }

    fn load_unlocked(&self) -> Result<bool, StoreError> {
        ensure_regular_or_missing(&self.path)?;
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(StoreError),
        };
        if file.metadata().map_err(|_| StoreError)?.len() > MAX_STATE_BYTES {
            return Err(StoreError);
        }
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| StoreError)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(StoreError);
        }
        if bytes.is_empty() {
            return Ok(false);
        }
        serde_json::from_slice::<StateFile>(&bytes)
            .map(|state| state.enabled)
            .map_err(|_| StoreError)
    }

    fn persist_unlocked(&self, enabled: bool) -> Result<(), StoreError> {
        ensure_regular_or_missing(&self.path)?;
        let parent = self.path.parent().ok_or(StoreError)?;
        let temporary = parent.join(format!(
            ".voice-enabled.{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| StoreError)?;
        let result = (|| {
            serde_json::to_writer(&mut file, &StateFile { enabled }).map_err(|_| StoreError)?;
            file.sync_all().map_err(|_| StoreError)?;
            ensure_regular_or_missing(&self.path)?;
            fs::rename(&temporary, &self.path).map_err(|_| StoreError)?;
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| StoreError)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

#[derive(Debug)]
struct StoreError;

/// Historical cached value only: no AT query, lock creation, or evidence write.
pub(crate) fn quarantined_status(data_root: &Path) -> Response<Full<Bytes>> {
    let enabled = Store::new(data_root).load_unlocked().unwrap_or(false);
    crate::json_response(
        StatusCode::OK,
        &json!({
            "enabled":enabled, "module_available":false,
            "code":crate::module_recovery::CODE,
            "historical_recovery_confirmed":false
        }),
    )
}

static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn ensure_regular_or_missing(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(StoreError),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(StoreError),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisabledRequest {
    #[serde(default)]
    enabled: bool,
}

pub(crate) async fn handle_public<B>(
    request: Request<B>,
    actor: crate::modem_actor::ModemActor,
    data_root: &Path,
) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/voice/enabled") => {
            let store = Store::new(data_root);
            let fallback_store = store.clone();
            let query = tokio::task::spawn_blocking(move || actor.query_voice_enabled())
                .await
                .ok()
                .flatten();
            let enabled = match query {
                Some(enabled) => {
                    store.save_best_effort(enabled);
                    enabled
                }
                None => fallback_store.load(),
            };
            crate::json_response(StatusCode::OK, &json!({"enabled": enabled}))
        }
        (&Method::POST, "/api/voice/enable") => {
            let body = match crate::http_server::collect_body(
                request.into_body(),
                MAX_REQUEST_BYTES,
            )
            .await
            {
                Ok(body) => body,
                Err(crate::http_server::BodyReadError::Timeout) => {
                    return crate::http_server::body_timeout_response();
                }
                Err(_) => {
                    return crate::json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({"error":"request body exceeds the size limit"}),
                    );
                }
            };
            let parsed: Option<DisabledRequest> = match serde_json::from_slice(&body) {
                Ok(parsed) => parsed,
                Err(error) => {
                    return crate::json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({"error":error.to_string()}),
                    );
                }
            };
            let _requested_state = parsed.map(|request| request.enabled).unwrap_or(false);
            crate::json_response(StatusCode::CONFLICT, &json!({"error": DISABLED_MESSAGE}))
        }
        _ => crate::json_response(StatusCode::NOT_FOUND, &json!({"error":"route not found"})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn response_json(response: Response<Full<Bytes>>) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap())
    }

    fn test_root() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "switch-voice-enabled-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    #[test]
    fn voice_state_preserves_legacy_object_shape_and_round_trips() {
        let root = test_root();
        let store = Store::new(&root);
        assert!(!store.load());
        store.save_best_effort(true);
        assert!(store.load());
        assert_eq!(
            fs::read_to_string(root.join("voice-enabled.json")).unwrap(),
            "{\"enabled\":true}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_or_non_boolean_legacy_state_falls_back_to_disabled() {
        let root = test_root();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("voice-enabled.json"), b"{\"enabled\":\"yes\"}").unwrap();
        assert!(!Store::new(&root).load());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_state_reader_ignores_unknown_fields_like_go_json_unmarshal() {
        let root = test_root();
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("voice-enabled.json"),
            b"{\"enabled\":true,\"future_setting\":1}",
        )
        .unwrap();
        assert!(Store::new(&root).load());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn get_uses_persisted_value_when_modem_query_is_unavailable() {
        let root = test_root();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("voice-enabled.json"), b"{\"enabled\":true}").unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/voice/enabled")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = handle_public(
            request,
            crate::modem_actor::ModemActor::new().unwrap(),
            &root,
        )
        .await;
        let (status, body) = response_json(response).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"enabled": true}));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn disabled_post_validates_json_then_preserves_legacy_conflict() {
        let root = test_root();
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/voice/enable")
            .body(Full::new(Bytes::from_static(b"{\"enabled\":true}")))
            .unwrap();
        let response = handle_public(
            request,
            crate::modem_actor::ModemActor::new().unwrap(),
            &root,
        )
        .await;
        let (status, body) = response_json(response).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, json!({"error": DISABLED_MESSAGE}));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn disabled_post_rejects_unknown_fields_before_reporting_deprecation() {
        let root = test_root();
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/voice/enable")
            .body(Full::new(Bytes::from_static(
                b"{\"enabled\":true,\"force\":true}",
            )))
            .unwrap();
        let response = handle_public(
            request,
            crate::modem_actor::ModemActor::new().unwrap(),
            &root,
        )
        .await;
        let (status, body) = response_json(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("unknown field"));
        let _ = fs::remove_dir_all(root);
    }
}
