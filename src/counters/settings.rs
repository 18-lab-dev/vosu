use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use super::model::{CompactValues, CounterSetting, SettingInput, SettingType};

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("invalid counter name")]
    InvalidName,
    #[error("body payload is not array")]
    InvalidPayload,
    #[error("counter folder does not exist")]
    MissingCounter,
    #[error("counter folder is a symbolic link or escapes the static root")]
    UnsafeCounterPath,
    #[error("settings file is a symbolic link")]
    UnsafeSettingsFile,
    #[error("__ingame__ settings must be a JSON object")]
    InvalidIngameValues,
    #[error("settings I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("settings JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
pub struct SettingsStore {
    static_root: PathBuf,
    settings_root: PathBuf,
}

impl SettingsStore {
    pub fn new(static_root: PathBuf, settings_root: PathBuf) -> Self {
        Self {
            static_root,
            settings_root,
        }
    }

    pub fn settings_root(&self) -> &Path {
        &self.settings_root
    }

    pub fn load(&self, name: &str) -> Result<(Vec<CounterSetting>, CompactValues), SettingsError> {
        validate_component(name)?;
        self.require_counter_directory(name)?;
        fs::create_dir_all(&self.settings_root)?;
        self.migrate_legacy(name)?;
        let definition = self.definition_path(name);
        let values_path = self.values_path(name);
        reject_symlink(&definition)?;
        reject_symlink(&values_path)?;
        if name != "__ingame__" && !definition.exists() {
            atomic_json(&definition, &Vec::<CounterSetting>::new())?;
        }
        if !values_path.exists() {
            atomic_json(&values_path, &CompactValues::new())?;
        }
        let settings = if name == "__ingame__" {
            Vec::new()
        } else {
            read_json_or(&definition, Vec::new())?
        };
        let values = read_json_or(&values_path, CompactValues::new())?;
        Ok((settings, values))
    }

    pub fn counter_values(&self, name: &str) -> Result<CompactValues, SettingsError> {
        let (settings, mut values) = self.load(name)?;
        for setting in settings {
            let mut value = values
                .get(&setting.unique_id)
                .cloned()
                .unwrap_or(setting.value);
            match setting.kind {
                SettingType::Number => value = coerce_number(value),
                SettingType::Checkbox => value = coerce_checkbox(value),
                SettingType::Header => {
                    values.remove(&setting.unique_id);
                    continue;
                }
                _ => {}
            }
            values.insert(setting.unique_id, value);
        }
        Ok(values)
    }

    pub fn prepare_user_values(
        &self,
        name: &str,
        payload: &[SettingInput],
    ) -> Result<CompactValues, SettingsError> {
        validate_component(name)?;
        if name == "__ingame__" {
            return Err(SettingsError::InvalidPayload);
        }
        let (settings, mut values) = self.load(name)?;
        for item in payload {
            let id = sanitize_id(&item.unique_id);
            let Some(setting) = settings.iter().find(|setting| setting.unique_id == id) else {
                continue;
            };
            let value = match setting.kind {
                SettingType::Number => coerce_number(item.value.clone()),
                SettingType::Checkbox => coerce_checkbox(item.value.clone()),
                _ => item.value.clone(),
            };
            values.insert(id, value);
        }
        Ok(values)
    }

    pub fn save_user_values(
        &self,
        name: &str,
        payload: &[SettingInput],
    ) -> Result<CompactValues, SettingsError> {
        let values = self.prepare_user_values(name, payload)?;
        atomic_json(&self.values_path(name), &values)?;
        Ok(values)
    }

    pub fn save_ingame_values(&self, values: &Value) -> Result<CompactValues, SettingsError> {
        let object = values
            .as_object()
            .ok_or(SettingsError::InvalidIngameValues)?
            .clone();
        fs::create_dir_all(&self.settings_root)?;
        self.migrate_legacy("__ingame__")?;
        reject_symlink(&self.values_path("__ingame__"))?;
        atomic_json(&self.values_path("__ingame__"), values)?;
        Ok(object)
    }

    pub fn save_developer_settings(
        &self,
        name: &str,
        mut settings: Vec<CounterSetting>,
    ) -> Result<Vec<CounterSetting>, SettingsError> {
        validate_component(name)?;
        if name == "__ingame__" {
            return Err(SettingsError::InvalidName);
        }
        self.require_counter_directory(name)?;
        for setting in &mut settings {
            setting.unique_id = sanitize_id(&setting.unique_id);
            setting.value = match setting.kind {
                SettingType::Number => coerce_number(setting.value.clone()),
                SettingType::Checkbox => coerce_checkbox(setting.value.clone()),
                _ => setting.value.clone(),
            };
        }
        let path = self.definition_path(name);
        reject_symlink(&path)?;
        fs::create_dir_all(path.parent().expect("definition has parent"))?;
        atomic_json(&path, &settings)?;
        Ok(settings)
    }

    pub fn values_path(&self, name: &str) -> PathBuf {
        self.settings_root.join(format!("{name}.values.json"))
    }
    fn definition_path(&self, name: &str) -> PathBuf {
        self.static_root.join(name).join("settings.json")
    }
    fn legacy_path(&self, name: &str) -> PathBuf {
        self.static_root.join(name).join("settings.values.json")
    }

    fn require_counter_directory(&self, name: &str) -> Result<(), SettingsError> {
        if name == "__ingame__" {
            return Ok(());
        }
        let path = self.static_root.join(name);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                SettingsError::MissingCounter
            } else {
                SettingsError::Io(error)
            }
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(SettingsError::UnsafeCounterPath);
        }
        let root = fs::canonicalize(&self.static_root)?;
        let canonical = fs::canonicalize(path)?;
        if canonical.parent() != Some(root.as_path()) {
            return Err(SettingsError::UnsafeCounterPath);
        }
        Ok(())
    }

    fn validate_legacy_parent(&self, name: &str) -> Result<(), SettingsError> {
        let parent = self.static_root.join(name);
        match fs::symlink_metadata(&parent) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(SettingsError::UnsafeCounterPath);
                }
                let root = fs::canonicalize(&self.static_root)?;
                let canonical = fs::canonicalize(parent)?;
                if canonical.parent() != Some(root.as_path()) {
                    return Err(SettingsError::UnsafeCounterPath);
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn migrate_legacy(&self, name: &str) -> Result<(), SettingsError> {
        let legacy = self.legacy_path(name);
        self.validate_legacy_parent(name)?;
        reject_symlink(&legacy)?;
        reject_symlink(&self.values_path(name))?;
        if !legacy.exists() {
            return Ok(());
        }
        let target = self.values_path(name);
        fs::copy(&legacy, &target)?;
        let old = legacy.with_file_name("settings.old-values.json");
        reject_symlink(&old)?;
        if old.exists() {
            fs::remove_file(&old)?;
        }
        fs::rename(legacy, old)?;
        Ok(())
    }
}

pub fn validate_component(name: &str) -> Result<(), SettingsError> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(component)), None)
            if !component.is_empty()
                && component.to_str() == Some(name)
                && name != "."
                && name != ".." =>
        {
            Ok(())
        }
        _ => Err(SettingsError::InvalidName),
    }
}

fn sanitize_id(id: &str) -> String {
    id.chars().filter(|ch| ch.is_ascii_alphanumeric()).collect()
}

fn coerce_number(value: Value) -> Value {
    match value {
        Value::Number(_) => value,
        Value::String(text) => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::from(0)),
        Value::Bool(value) => Value::from(if value { 1 } else { 0 }),
        _ => Value::from(0),
    }
}

fn coerce_checkbox(value: Value) -> Value {
    match value {
        Value::String(text) => Value::Bool(text == "true"),
        other => other,
    }
}

fn read_json_or<T>(path: &Path, default: T) -> Result<T, SettingsError>
where
    T: serde::de::DeserializeOwned,
{
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or(default)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(default),
        Err(error) => Err(error.into()),
    }
}

fn reject_symlink(path: &Path) -> Result<(), SettingsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(SettingsError::UnsafeSettingsFile),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), SettingsError> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "settings path has no parent")
    })?;
    reject_symlink(path)?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".settings-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    serde_json::to_writer(&mut temp, value)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    let (file, temp_path) = temp.keep().map_err(|error| error.error)?;
    // Windows does not allow replacing a file while this process still owns a
    // non-shareable handle to it. Close the temporary file before the rename,
    // matching the ownership boundary used by the other atomic writers.
    drop(file);
    if let Err(error) = crate::platform::atomic_replace(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error.into());
    }
    sync_parent(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}
#[cfg(not(unix))]
fn sync_parent(_: &Path) -> io::Result<()> {
    Ok(())
}

#[allow(dead_code)]
fn _open_no_follow(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).open(path)
}
