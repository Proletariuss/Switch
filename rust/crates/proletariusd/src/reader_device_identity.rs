//! Rust-owned API and persistence for the stable reader IMS device identity.
//! The file name and JSON shape remain compatible with existing installations.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Body;
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use zeroize::{Zeroize, Zeroizing};

const MAX_CONFIG_BYTES: u64 = 4 * 1024;
const MAX_REQUEST_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub(crate) struct Store {
    path: PathBuf,
}

impl Store {
    pub(crate) fn new(data_root: &Path) -> Self {
        Self {
            path: data_root.join("imei.json"),
        }
    }

    fn load_raw(&self) -> Result<Option<Zeroizing<String>>, PersistedDeviceIdentityError> {
        ensure_regular_or_missing(&self.path)?;
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(PersistedDeviceIdentityError::Unreadable),
        };
        let size = file
            .metadata()
            .map_err(|_| PersistedDeviceIdentityError::Unreadable)?
            .len();
        if size > MAX_CONFIG_BYTES {
            return Err(PersistedDeviceIdentityError::Oversized);
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(size as usize));
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| PersistedDeviceIdentityError::Unreadable)?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(PersistedDeviceIdentityError::Oversized);
        }
        Ok(parse_imei_config(&bytes))
    }

    fn load(&self) -> Result<Option<Zeroizing<String>>, PersistedDeviceIdentityError> {
        Ok(self.load_raw()?.and_then(|imei| {
            let trimmed = imei.trim();
            (!trimmed.is_empty()).then(|| Zeroizing::new(trimmed.to_owned()))
        }))
    }

    fn save(&self, imei: &str) -> Result<(), PersistedDeviceIdentityError> {
        let _lock = self.lock()?;
        self.save_locked(imei)
    }

    fn save_locked(&self, imei: &str) -> Result<(), PersistedDeviceIdentityError> {
        ensure_regular_or_missing(&self.path)?;
        let parent = self
            .path
            .parent()
            .ok_or(PersistedDeviceIdentityError::Unreadable)?;
        let file_name = self
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(PersistedDeviceIdentityError::Unreadable)?;
        let temporary = parent.join(format!(
            ".{file_name}.{}-{}.tmp",
            std::process::id(),
            crate::snapshot::new_instance_id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| PersistedDeviceIdentityError::Unreadable)?;
        let result = (|| {
            let bytes = serde_json::to_vec_pretty(&ImeiConfig {
                imei: imei.to_owned(),
            })
            .map_err(|_| PersistedDeviceIdentityError::Unreadable)?;
            file.write_all(&bytes)
                .map_err(|_| PersistedDeviceIdentityError::Unreadable)?;
            file.sync_all()
                .map_err(|_| PersistedDeviceIdentityError::Unreadable)?;
            ensure_regular_or_missing(&self.path)?;
            fs::rename(&temporary, &self.path)
                .map_err(|_| PersistedDeviceIdentityError::Unreadable)?;
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| PersistedDeviceIdentityError::Unreadable)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub(crate) fn resolve_or_create(&self) -> Result<String, PersistedDeviceIdentityError> {
        // Serialize first-run generation with explicit GET/PUT/DELETE users.
        // Otherwise two concurrent VoWiFi starts could advertise different
        // device identities while only one value wins the atomic rename.
        let _lock = self.lock()?;
        if let Some(existing) = self.load_raw()? {
            let value = existing.trim();
            if !value.is_empty() {
                return Ok(value.to_owned());
            }
        }
        let generated = generate_imei()?;
        self.save_locked(&generated)?;
        Ok(generated)
    }

    fn lock(&self) -> Result<File, PersistedDeviceIdentityError> {
        let parent = self
            .path
            .parent()
            .ok_or(PersistedDeviceIdentityError::Unreadable)?;
        fs::create_dir_all(parent).map_err(|_| PersistedDeviceIdentityError::Unreadable)?;
        if fs::symlink_metadata(parent)
            .map(|metadata| !metadata.is_dir() || metadata.file_type().is_symlink())
            .unwrap_or(true)
        {
            return Err(PersistedDeviceIdentityError::Unreadable);
        }
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|_| PersistedDeviceIdentityError::Unreadable)?;
        let lock_path = self.path.with_extension("json.lock");
        ensure_regular_or_missing(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|_| PersistedDeviceIdentityError::Unreadable)?;
        lock.lock()
            .map_err(|_| PersistedDeviceIdentityError::Unreadable)?;
        Ok(lock)
    }
}

#[derive(Debug)]
pub(crate) enum PersistedDeviceIdentityError {
    Unreadable,
    Oversized,
}

/// Loads the stable identity for native IMS session preparation. Missing,
/// empty, or malformed JSON remains an unavailable identity; callers must not
/// invent a replacement because carrier-side terminating-call binding depends
/// on stability.
pub(crate) fn load_persisted_imei()
-> Result<Option<Zeroizing<String>>, PersistedDeviceIdentityError> {
    Store::new(&crate::data_root::resolve().map_err(|_| PersistedDeviceIdentityError::Unreadable)?)
        .load()
}

pub(crate) const INTERNAL_RESOLVE_PATH: &str = "/api/internal/reader-device-identity/resolve";

/// Resolve the existing configured identity or create it once. This is the
/// Go compatibility session's only access to the Rust-owned persisted value.
pub(crate) async fn handle_internal(
    request: Request<Incoming>,
    data_root: &Path,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST || request.uri().path() != INTERNAL_RESOLVE_PATH {
        return crate::json_response(
            StatusCode::NOT_FOUND,
            &json!({"code":"route_not_found","error":"route not found","retryable":false}),
        );
    }
    let bytes = match crate::http_server::collect_body(request.into_body(), MAX_REQUEST_BYTES).await
    {
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Ok(body) => body,
        Err(_) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"code":"invalid_request_body","error":"请求格式错误","retryable":false}),
            );
        }
    };
    if serde_json::from_slice::<ResolveRequest>(&bytes).is_err() {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"code":"invalid_request_body","error":"请求格式错误","retryable":false}),
        );
    }
    match Store::new(data_root).resolve_or_create() {
        Ok(imei) => crate::json_response(StatusCode::OK, &json!({"imei":imei})),
        Err(_) => crate::json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"code":"reader_imei_unavailable","error":"IMEI 读取或初始化失败","retryable":true}),
        ),
    }
}

fn generate_imei() -> Result<String, PersistedDeviceIdentityError> {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|_| PersistedDeviceIdentityError::Unreadable)?;
    // Preserve the app's stable 353936 TAC family and existing 15-digit
    // format; only the entropy source and persistence owner move to Rust.
    let tac_suffix = u16::from_be_bytes([random[0], random[1]]) % 100;
    let serial = u32::from_be_bytes([random[2], random[3], random[4], random[5]]) % 1_000_000;
    let first_fourteen = format!("353936{tac_suffix:02}{serial:06}");
    let check = luhn_check_digit(&first_fourteen);
    Ok(format!("{first_fourteen}{check}"))
}

fn luhn_check_digit(digits: &str) -> u8 {
    let sum = digits
        .bytes()
        .enumerate()
        .map(|(index, byte)| {
            let mut value = byte - b'0';
            if index % 2 == 1 {
                value *= 2;
                if value > 9 {
                    value -= 9;
                }
            }
            value
        })
        .map(u32::from)
        .sum::<u32>();
    ((10 - sum % 10) % 10) as u8
}

fn ensure_regular_or_missing(path: &Path) -> Result<(), PersistedDeviceIdentityError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(PersistedDeviceIdentityError::Unreadable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(PersistedDeviceIdentityError::Unreadable),
    }
}

fn parse_imei_config(bytes: &[u8]) -> Option<Zeroizing<String>> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    // The on-disk buffer is zeroized at scope exit. After a successful Serde
    // decode, explicitly zero the temporary `String` after copying the IMS
    // identity into a guarded return type.
    let mut configuration = serde_json::from_slice::<ImeiConfig>(bytes).ok()?;
    let result =
        (!configuration.imei.is_empty()).then(|| Zeroizing::new(configuration.imei.clone()));
    configuration.imei.zeroize();
    result
}

#[derive(Deserialize, Serialize)]
struct ImeiConfig {
    #[serde(default)]
    imei: String,
}

#[derive(Deserialize)]
struct SetImeiRequest {
    #[serde(default)]
    imei: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveRequest {}

pub(crate) async fn handle_public<B>(request: Request<B>, store: Store) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/reader/imei") => match store.load_raw() {
            Ok(imei) => {
                let imei = imei.as_ref().map(|value| value.as_str()).unwrap_or("");
                crate::json_response(
                    StatusCode::OK,
                    &json!({"imei":imei,"config":!imei.is_empty()}),
                )
            }
            Err(_) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"error":"IMEI 读取失败"}),
            ),
        },
        (&Method::PUT, "/api/reader/imei") => {
            let body = match decode_request(request).await {
                Ok(body) => body,
                Err(response) => return response,
            };
            let imei = body.imei.unwrap_or_default().trim().to_owned();
            if !imei.is_empty()
                && (imei.len() != 15 || !imei.bytes().all(|digit| digit.is_ascii_digit()))
            {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":"IMEI 必须为 15 位数字"}),
                );
            }
            match store.save(&imei) {
                Ok(()) => crate::json_response(
                    StatusCode::OK,
                    &json!({"imei":imei,"message":"IMEI 已保存"}),
                ),
                Err(_) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &json!({"error":"IMEI 保存失败"}),
                ),
            }
        }
        (&Method::DELETE, "/api/reader/imei") => match store.save("") {
            Ok(()) => crate::json_response(StatusCode::OK, &json!({"message":"IMEI 已清除"})),
            Err(_) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"error":"IMEI 清除失败"}),
            ),
        },
        _ => json_response(StatusCode::NOT_FOUND, &json!({"error":"route not found"})),
    }
}

async fn decode_request<B>(request: Request<B>) -> Result<SetImeiRequest, Response<Full<Bytes>>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let bytes = match crate::http_server::collect_body(request.into_body(), MAX_REQUEST_BYTES).await
    {
        Err(crate::http_server::BodyReadError::Timeout) => {
            return Err(crate::http_server::body_timeout_response());
        }
        Ok(body) => body,
        Err(_) => {
            return Err(json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"请求格式错误"}),
            ));
        }
    };
    let mut decoder = serde_json::Deserializer::from_slice(&bytes);
    Option::<SetImeiRequest>::deserialize(&mut decoder)
        .map(|value| value.unwrap_or(SetImeiRequest { imei: None }))
        .map_err(|_| json_response(StatusCode::BAD_REQUEST, &json!({"error":"请求格式错误"})))
}

fn json_response(status: StatusCode, value: &serde_json::Value) -> Response<Full<Bytes>> {
    crate::json_response(status, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use hyper::Request;
    use serde_json::Value;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::thread;

    fn test_root() -> PathBuf {
        let path = std::path::PathBuf::from(
            std::env::var_os("SWITCH_BUILD_DIR").expect("workspace-env build directory"),
        )
        .join("tmp/reader-imei-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            crate::snapshot::new_instance_id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn request(method: Method, body: &str) -> Request<Full<Bytes>> {
        Request::builder()
            .method(method)
            .uri("/api/reader/imei")
            .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
            .unwrap()
    }

    async fn response_json(response: Response<Full<Bytes>>) -> (StatusCode, Value) {
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[test]
    fn parses_the_existing_shared_shape_without_changing_api_whitespace() {
        let raw = parse_imei_config(br#"{"imei":" 353936932685355 "}"#).unwrap();
        assert_eq!(raw.as_str(), " 353936932685355 ");
    }

    #[test]
    fn missing_or_malformed_compatibility_config_is_not_replaced() {
        for input in [b"".as_slice(), b"{}", b"{not-json}", b"{\"imei\":\"\"}"] {
            assert!(parse_imei_config(input).is_none());
        }
    }

    #[test]
    fn fallback_path_keeps_the_go_compatibility_filename() {
        assert!(
            Store::new(Path::new("/fixture/data"))
                .path
                .ends_with("imei.json")
        );
    }

    #[tokio::test]
    async fn public_imei_api_preserves_wire_shapes_validation_and_shared_file() {
        let root = test_root();
        let store = Store::new(&root);
        let (status, value) =
            response_json(handle_public(request(Method::GET, ""), store.clone()).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value, json!({"imei":"","config":false}));
        assert!(
            !root.join("imei.json.lock").exists(),
            "GET must not create config files"
        );

        let (status, value) = response_json(
            handle_public(
                request(
                    Method::PUT,
                    r#"{"imei":" 353936932685355 "} {"ignored":true}"#,
                ),
                store.clone(),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            value,
            json!({"imei":"353936932685355","message":"IMEI 已保存"})
        );
        assert_eq!(
            fs::read_to_string(root.join("imei.json")).unwrap(),
            "{\n  \"imei\": \"353936932685355\"\n}"
        );
        assert_eq!(
            fs::metadata(root.join("imei.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(root.join("imei.json.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let (status, value) =
            response_json(handle_public(request(Method::GET, ""), store.clone()).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value, json!({"imei":"353936932685355","config":true}));

        for invalid in [
            r#"{"imei":"1234"}"#,
            r#"{"imei":"12345678901234x"}"#,
            r#"{"imei":"１２３４５６７８９０１２３４５"}"#,
            "not-json",
        ] {
            let (status, value) =
                response_json(handle_public(request(Method::PUT, invalid), store.clone()).await)
                    .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
            assert!(value.get("error").is_some(), "{invalid}");
        }

        let (status, value) =
            response_json(handle_public(request(Method::DELETE, ""), store.clone()).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value, json!({"message":"IMEI 已清除"}));
        assert_eq!(store.load().unwrap(), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn writes_are_serialized_and_existing_corrupt_identity_is_replaceable() {
        let root = test_root();
        fs::write(root.join("imei.json"), b"{not-json").unwrap();
        let store = Store::new(&root);
        let writers = (0..4)
            .map(|index| {
                let store = store.clone();
                thread::spawn(move || {
                    store.save(&format!("3539369326853{index:02}")).unwrap();
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().unwrap();
        }
        let value = store.load().unwrap().unwrap();
        assert_eq!(value.len(), 15);
        assert!(value.bytes().all(|digit| digit.is_ascii_digit()));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn first_use_generates_one_stable_luhn_identity_under_concurrency() {
        let root = test_root();
        let workers = (0..8)
            .map(|_| {
                let root = root.clone();
                thread::spawn(move || Store::new(&root).resolve_or_create().unwrap())
            })
            .collect::<Vec<_>>();
        let values = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert!(values.iter().all(|value| value == &values[0]));
        let value = &values[0];
        assert_eq!(value.len(), 15);
        assert!(value.starts_with("353936"));
        assert!(value.bytes().all(|digit| digit.is_ascii_digit()));
        assert_eq!(value.as_bytes()[14] - b'0', luhn_check_digit(&value[..14]));
        let persisted = Store::new(&root).load().unwrap();
        assert_eq!(
            persisted.as_ref().map(|value| value.as_str()),
            Some(value.as_str())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_observes_a_publicly_configured_identity_and_regenerates_after_clear() {
        let root = test_root();
        let store = Store::new(&root);
        store.save("353936932685355").unwrap();
        assert_eq!(store.resolve_or_create().unwrap(), "353936932685355");
        store.save("").unwrap();
        let generated = store.resolve_or_create().unwrap();
        assert_eq!(generated.len(), 15);
        assert_eq!(store.resolve_or_create().unwrap(), generated);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn imei_store_rejects_symlink_without_touching_target() {
        use std::os::unix::fs::symlink;

        let root = test_root();
        let outside = root.with_extension("imei.json");
        fs::write(&outside, br#"{"imei":"untouched"}"#).unwrap();
        symlink(&outside, root.join("imei.json")).unwrap();
        let store = Store::new(&root);
        assert!(store.save("353936932685355").is_err());
        assert_eq!(fs::read(&outside).unwrap(), br#"{"imei":"untouched"}"#);
        let _ = fs::remove_file(root.join("imei.json"));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_file(outside);
    }
}
