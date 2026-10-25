//! Rust-owned persistence and API for locally entered eSIM profile notes.
//!
//! The JSON object shape intentionally matches the existing Go writer's
//! `profile-notes.json` file so a cutover preserves user data in place.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Body;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub(crate) const INTERNAL_PREFIX: &str = "/api/internal/profile-notes/";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ProfileNote {
    #[serde(default)]
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) phone: String,
    #[serde(default)]
    pub(crate) tags: String,
}

/// Keep unrecognised persisted fields opaque. Public note DTOs and requests
/// remain limited to the three fields this version understands.
#[derive(Default, Serialize)]
struct StoredProfileNote {
    #[serde(flatten)]
    note: ProfileNote,
    #[serde(flatten)]
    extensions: BTreeMap<String, Box<serde_json::value::RawValue>>,
}

#[derive(Clone)]
pub(crate) struct Store {
    path: PathBuf,
    local_lock: Arc<Mutex<()>>,
    #[cfg(test)]
    read_probe: Option<Arc<read_offload_tests::ReadProbe>>,
}

impl Store {
    pub(crate) fn new(data_root: &Path) -> Self {
        Self {
            path: data_root.join("profile-notes.json"),
            local_lock: Arc::new(Mutex::new(())),
            #[cfg(test)]
            read_probe: None,
        }
    }

    fn load(&self) -> Result<BTreeMap<String, ProfileNote>, StoreError> {
        self.load_unlocked().map(|notes| {
            notes
                .into_iter()
                .map(|(key, stored)| (key, stored.note))
                .collect()
        })
    }

    fn save_note(&self, iccid: String, note: ProfileNote) -> Result<ProfileNote, StoreError> {
        self.with_lock(|_| {
            let mut notes = self.load_unlocked()?;
            let stored = notes.entry(iccid.clone()).or_default();
            stored.note = note.clone();
            if note.label.is_empty() && note.phone.is_empty() && note.tags.is_empty() {
                // Clearing the known fields must not erase data written by a
                // newer version. A legacy note without extensions still deletes.
                if stored.extensions.is_empty() {
                    notes.remove(&iccid);
                }
                self.persist_unlocked(&notes)?;
                return Ok(ProfileNote::default());
            }
            self.persist_unlocked(&notes)?;
            Ok(note)
        })
    }

    fn phone_for_iccid(&self, iccid: &str) -> Result<String, StoreError> {
        let notes = self.load_unlocked()?;
        let target = normalize_sim_identifier(iccid);
        if target.is_empty() {
            return Ok(String::new());
        }
        Ok(notes
            .iter()
            .find(|(key, _)| normalize_sim_identifier(key) == target)
            .map(|(_, stored)| stored.note.phone.trim())
            .filter(|phone| !is_missing_phone(phone))
            .unwrap_or_default()
            .to_owned())
    }

    pub(crate) fn phone_for_iccid_or_empty(&self, iccid: &str) -> String {
        self.phone_for_iccid(iccid).unwrap_or_default()
    }

    /// Native REGISTER cache: preserve existing note keys and user label/tags.
    /// Admission is checked under the file lock immediately before commit work.
    /// Once admitted, an eventual I/O error is not evidence that rename never ran.
    pub(crate) fn remember_registered_phone(
        &self,
        iccid: &str,
        phone: &str,
        admit: impl FnOnce() -> bool,
    ) -> Result<Option<bool>, &'static str> {
        if normalize_sim_identifier(iccid).is_empty() || is_missing_phone(phone) {
            return Ok(None);
        }
        self.with_lock(|_| {
            let mut notes = self.load_unlocked()?;
            let key = match unique_profile_key(&notes, iccid) {
                Ok(key) => key.unwrap_or_else(|| iccid.trim().to_owned()),
                Err(code) => return Ok(Err(code)),
            };
            if !admit() {
                return Ok(Ok(None));
            }
            let note = &mut notes.entry(key).or_default().note;
            if note.phone == phone.trim() {
                return Ok(Ok(Some(false)));
            }
            note.phone = phone.trim().to_owned();
            self.persist_unlocked(&notes)?;
            Ok(Ok(Some(true)))
        })
        .map_err(|_| "native_registered_phone_store_unavailable")?
    }
    pub(crate) fn registered_phone_fallback(&self, iccid: &str) -> Result<String, &'static str> {
        let notes = self
            .load_unlocked()
            .map_err(|_| "native_registered_phone_store_unavailable")?;
        let Some(key) = unique_profile_key(&notes, iccid)? else {
            return Ok(String::new());
        };
        Ok(notes[&key].note.phone.trim().to_owned())
    }

    fn remember_phone(&self, iccid: &str, phone: &str) -> Result<bool, StoreError> {
        let iccid = iccid.trim();
        let phone = phone.trim();
        if normalize_sim_identifier(iccid).is_empty() || is_missing_phone(phone) {
            return Ok(false);
        }
        self.with_lock(|_| {
            let mut notes = self.load_unlocked()?;
            let note = &mut notes.entry(iccid.to_owned()).or_default().note;
            if note.phone == phone {
                return Ok(false);
            }
            note.phone = phone.to_owned();
            self.persist_unlocked(&notes)?;
            Ok(true)
        })
    }

    fn with_lock<T>(
        &self,
        operation: impl FnOnce(&File) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let _local = self
            .local_lock
            .lock()
            .map_err(|_| StoreError::Unavailable)?;
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
        operation(&lock)
    }

    fn load_unlocked(&self) -> Result<BTreeMap<String, StoredProfileNote>, StoreError> {
        #[cfg(test)]
        let _read_owner = self
            .read_probe
            .as_deref()
            .map(read_offload_tests::ReadProbe::enter);
        ensure_regular_or_missing(&self.path)?;
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
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
        if bytes.is_empty() {
            return Ok(BTreeMap::new());
        }
        // Parse entries and extensions as raw JSON: Value would round unknown
        // large integers/precise decimals before another field is edited.
        let entries: Option<BTreeMap<String, Box<serde_json::value::RawValue>>> =
            serde_json::from_slice(&bytes).map_err(|_| StoreError::Unavailable)?;
        entries
            .unwrap_or_default()
            .into_iter()
            .map(|(key, raw)| {
                let mut extensions: BTreeMap<String, Box<serde_json::value::RawValue>> =
                    serde_json::from_str(raw.get()).map_err(|_| StoreError::Unavailable)?;
                // The previous Value-based loader kept the last duplicate
                // object key. Preserve that rule without parsing opaque data.
                let mut known = |field: &str| match extensions.remove(field) {
                    Some(value) => serde_json::from_str::<String>(value.get())
                        .map_err(|_| StoreError::Unavailable),
                    None => Ok(String::new()),
                };
                let note = ProfileNote {
                    label: known("label")?,
                    phone: known("phone")?,
                    tags: known("tags")?,
                };
                Ok((key, StoredProfileNote { note, extensions }))
            })
            .collect()
    }

    fn persist_unlocked(
        &self,
        notes: &BTreeMap<String, StoredProfileNote>,
    ) -> Result<(), StoreError> {
        ensure_regular_or_missing(&self.path)?;
        let parent = self.path.parent().ok_or(StoreError::Unavailable)?;
        let file_name = self
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(StoreError::Unavailable)?;
        let temporary = parent.join(format!(
            ".{file_name}.{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| StoreError::Unavailable)?;
        let result = (|| {
            let bytes = serde_json::to_vec_pretty(notes).map_err(|_| StoreError::Unavailable)?;
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err(StoreError::Unavailable);
            }
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

#[derive(Debug)]
enum StoreError {
    Unavailable,
    NotRegularFile,
}

static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn ensure_regular_or_missing(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(StoreError::NotRegularFile),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(StoreError::Unavailable),
    }
}

fn unique_profile_key(
    notes: &BTreeMap<String, StoredProfileNote>,
    iccid: &str,
) -> Result<Option<String>, &'static str> {
    let target = normalize_sim_identifier(iccid);
    if target.is_empty() {
        return Ok(None);
    }
    let mut keys = notes
        .keys()
        .filter(|key| normalize_sim_identifier(key) == target);
    let first = keys.next().cloned();
    if keys.next().is_some() {
        return Err("native_registered_phone_ambiguous_iccid");
    }
    Ok(first)
}

fn normalize_sim_identifier(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_end_matches(['F', 'f'])
        .to_owned()
}

pub(crate) fn is_missing_phone(phone: &str) -> bool {
    let phone = phone.trim();
    if phone.is_empty()
        || matches!(
            phone.to_ascii_lowercase().as_str(),
            "-" | "unknown" | "unknown number" | "n/a" | "号码未知" | "未知号码"
        )
    {
        return true;
    }
    let digits = phone
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect::<String>();
    digits.is_empty() || digits.chars().all(|character| character == '0')
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveNoteRequest {
    #[serde(default)]
    iccid: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    phone: String,
    #[serde(default)]
    tags: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PhoneRequest {
    iccid: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RememberPhoneRequest {
    iccid: String,
    phone: String,
}

// Preserve the public endpoint errors while retaining failed abandoned reads
// in the archive worker ledger. No private path or file content is exposed.
fn read_error(_: StoreError) -> proletarius_store::StoreError {
    proletarius_store::StoreError::Configuration("profile_notes_unavailable".into())
}

pub(crate) async fn handle_public<B>(request: Request<B>, store: Store) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/esim/notes") => {
            match crate::archive_io::run(move || store.load().map_err(read_error)).await {
                Ok(notes) => crate::json_response(StatusCode::OK, &json!({"notes": notes})),
                Err(_) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &json!({"error":"本地备注读取失败"}),
                ),
            }
        }
        (&Method::PUT, "/api/esim/notes") => {
            let body: SaveNoteRequest = match decode_json(request).await {
                Ok(body) => body,
                Err(response) => return response,
            };
            let iccid = body.iccid.trim().to_owned();
            let note = ProfileNote {
                label: body.label.trim().to_owned(),
                phone: body.phone.trim().to_owned(),
                tags: body.tags.trim().to_owned(),
            };
            if iccid.is_empty() {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":"iccid is required"}),
                );
            }
            if note.label.chars().count() > 80
                || note.phone.chars().count() > 80
                || note.tags.chars().count() > 200
            {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":"本地备注字段过长"}),
                );
            }
            match store.save_note(iccid, note) {
                Ok(note) => crate::json_response(
                    StatusCode::OK,
                    &json!({"message":"本地备注已保存","note":note}),
                ),
                Err(_) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &json!({"error":"本地备注保存失败"}),
                ),
            }
        }
        _ => json_response(StatusCode::NOT_FOUND, &json!({"error":"route not found"})),
    }
}

pub(crate) async fn handle_internal<B>(request: Request<B>, store: Store) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    match (request.method(), request.uri().path()) {
        (&Method::POST, "/api/internal/profile-notes/phone") => {
            let body: PhoneRequest = match decode_json(request).await {
                Ok(body) => body,
                Err(response) => return response,
            };
            match crate::archive_io::run(move || {
                store.phone_for_iccid(&body.iccid).map_err(read_error)
            })
            .await
            {
                Ok(phone) => crate::json_response(StatusCode::OK, &json!({"phone": phone})),
                Err(_) => json_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &json!({"code":"profile_notes_unavailable","error":"profile notes are unavailable"}),
                ),
            }
        }
        (&Method::POST, "/api/internal/profile-notes/remember-phone") => {
            let body: RememberPhoneRequest = match decode_json(request).await {
                Ok(body) => body,
                Err(response) => return response,
            };
            match store.remember_phone(&body.iccid, &body.phone) {
                Ok(_) => crate::json_response(StatusCode::OK, &json!({"ok":true})),
                Err(_) => json_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &json!({"code":"profile_notes_unavailable","error":"profile notes are unavailable"}),
                ),
            }
        }
        _ => json_response(
            StatusCode::NOT_FOUND,
            &json!({"code":"route_not_found","error":"route not found"}),
        ),
    }
}

async fn decode_json<B, T>(request: Request<B>) -> Result<T, Response<Full<Bytes>>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    T: for<'de> Deserialize<'de>,
{
    let body = match crate::http_server::collect_body(request.into_body(), MAX_REQUEST_BYTES).await
    {
        Ok(body) => body,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return Err(crate::http_server::body_timeout_response());
        }
        Err(_) => {
            return Err(json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"请求内容无效或超过大小限制"}),
            ));
        }
    };
    serde_json::from_slice(&body).map_err(|error| {
        json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":format!("invalid JSON request: {error}")}),
        )
    })
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response<Full<Bytes>> {
    crate::json_response(status, value)
}

#[cfg(test)]
mod tests {
    include!("profile_notes_tests.rs");
}

#[cfg(test)]
#[path = "profile_notes_read_offload_tests.rs"]
mod read_offload_tests;
