//! Sole writer of the legacy object-shaped module-flight-mode preference file.
//! This intentionally does not wrap ArchiveStore, whose root is a JSON array.
use proletarius_store::StoreError;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 16 * 1024;
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Preferences {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) restore_cfun: i32,
    #[serde(default)]
    pub(crate) user_configured: bool,
}
impl Preferences {
    pub(crate) fn suppressed(self) -> bool {
        !self.user_configured || self.enabled
    }
    fn validate(self) -> Result<Self, StoreError> {
        if self.enabled && !(0..=3).contains(&self.restore_cfun) {
            return Err(StoreError::Configuration(
                "invalid saved CFUN restore mode".into(),
            ));
        }
        Ok(self)
    }
}
#[derive(Clone)]
pub(crate) struct Store {
    path: PathBuf,
}
impl Store {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            path: root.join("module-flight-mode.json"),
        }
    }
    pub(crate) fn load(&self) -> Result<Preferences, StoreError> {
        let _lock = self.lock()?;
        self.load_unlocked()
    }
    fn load_unlocked(&self) -> Result<Preferences, StoreError> {
        regular_or_missing(&self.path)?;
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Preferences::default());
            }
            Err(error) => return Err(StoreError::Io(error)),
        };
        let mut data = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(StoreError::Io)?;
        if data.len() as u64 > MAX_BYTES {
            return Err(StoreError::Configuration(
                "module radio preferences exceed limit".into(),
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(&data).map_err(StoreError::Json)?;
        if !value.is_object() {
            return Err(StoreError::Configuration(
                "module radio preferences must be an object".into(),
            ));
        }
        serde_json::from_value::<Preferences>(value)
            .map_err(StoreError::Json)?
            .validate()
    }
    pub(crate) fn save(&self, preferences: Preferences) -> Result<(), StoreError> {
        preferences.validate()?;
        let _lock = self.lock()?;
        // Never overwrite a corrupt or unexpected existing preference file.
        self.load_unlocked()?;
        let parent = self.path.parent().ok_or(StoreError::MissingParent)?;
        let temporary = parent.join(format!(
            ".module-flight-mode-{}.tmp",
            crate::snapshot::new_instance_id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(StoreError::Io)?;
        let result = (|| {
            file.write_all(&serde_json::to_vec(&preferences).map_err(StoreError::Json)?)
                .map_err(StoreError::Io)?;
            file.sync_all().map_err(StoreError::Io)?;
            regular_or_missing(&self.path)?;
            fs::rename(&temporary, &self.path).map_err(StoreError::Io)?;
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(StoreError::Io)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    fn lock(&self) -> Result<File, StoreError> {
        let parent = self.path.parent().ok_or(StoreError::MissingParent)?;
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(StoreError::Io)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(StoreError::Io)?;
        }
        let path = self.path.with_extension("json.lock");
        regular_or_missing(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .map_err(StoreError::Io)?;
        file.lock().map_err(StoreError::Io)?;
        Ok(file)
    }
}
fn regular_or_missing(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(StoreError::NotRegularFile),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StoreError::Io(error)),
    }
}
#[cfg(test)]
mod tests {
    include!("module_radio_store_tests.rs");
}
