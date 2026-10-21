use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::model::{CompactValues, CounterSetting, SettingInput, SettingType};

static PROFILE_MUTATIONS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("invalid counter name")]
    InvalidName,
    #[error("counter folder does not exist")]
    MissingCounter,
    #[error("counter folder is unsafe or escapes static root")]
    UnsafeCounterPath,
    #[error("settings file is a symbolic link")]
    UnsafeSettingsFile,
    #[error("settings I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("settings JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("settings TOML parsing failed: {0}")]
    TomlParse(#[from] toml::de::Error),
    #[error("settings TOML serialization failed: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
    #[error("settings TOML conversion failed: {0}")]
    TomlConversion(String),
    #[error("{0}")]
    Profile(String),
}

#[derive(Clone, Debug)]
pub struct SettingsStore {
    static_root: PathBuf,
    settings_root: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profiles {
    pub active: String,
    pub profiles: Vec<String>,
}

pub fn qualified_counter(name: &str, profile: &str) -> String {
    format!("/@vosu-profile/{profile}/{name}")
}

pub fn split_counter(name: &str) -> (&str, Option<&str>) {
    if let Some(rest) = name.strip_prefix("/@vosu-profile/")
        && let Some((profile, overlay)) = rest.split_once('/')
    {
        return (overlay, Some(profile));
    }
    (name, None)
}

fn validate_profile(name: &str) -> Result<(), SettingsError> {
    if name.is_empty()
        || name.len() > 48
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(SettingsError::Profile("invalid profile name".into()));
    }
    Ok(())
}

#[derive(Deserialize)]
struct SchemaDocument {
    settings: Vec<CounterSetting>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceState {
    Missing,
    Present,
    Symlink,
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

    pub fn profiles(&self) -> Result<Profiles, SettingsError> {
        let path = self.settings_root.join("profiles.json");
        if !checked_exists(&path)? {
            return Ok(Profiles {
                active: "default".into(),
                profiles: vec!["default".into()],
            });
        }
        let registry: Profiles = read_json(&path)?;
        if registry.profiles.is_empty()
            || registry.profiles.len() > 32
            || !registry.profiles.iter().any(|p| p == "default")
            || !registry.profiles.contains(&registry.active)
        {
            return Err(SettingsError::Profile("invalid profile registry".into()));
        }
        let mut names = std::collections::HashSet::new();
        for name in &registry.profiles {
            validate_profile(name)?;
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(SettingsError::Profile("duplicate profile name".into()));
            }
        }
        Ok(registry)
    }

    pub fn resolve_profile(&self, profile: Option<&str>) -> Result<String, SettingsError> {
        let registry = self.profiles()?;
        let name = profile.unwrap_or(&registry.active);
        if !registry.profiles.iter().any(|p| p == name) {
            return Err(SettingsError::Profile("unknown profile".into()));
        }
        Ok(name.to_owned())
    }

    fn profile_root(&self, profile: &str) -> Result<PathBuf, SettingsError> {
        if profile == "default" {
            return Ok(self.settings_root.clone());
        }
        let parent = self.settings_root.join("profiles");
        reject_symlink(&parent)?;
        let path = parent.join(profile);
        reject_symlink(&path)?;
        Ok(path)
    }

    pub fn manage_profile(
        &self,
        action: &str,
        name: &str,
        target: Option<&str>,
    ) -> Result<Profiles, SettingsError> {
        let _guard = PROFILE_MUTATIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fs::create_dir_all(&self.settings_root)?;
        validate_profile(name)?;
        let mut registry = self.profiles()?;
        match action {
            "create" | "rename" => {
                let new_name = if action == "rename" {
                    target.unwrap_or("")
                } else {
                    name
                };
                validate_profile(new_name)?;
                if registry
                    .profiles
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(new_name))
                {
                    return Err(SettingsError::Profile("profile already exists".into()));
                }
                if action == "create" {
                    if registry.profiles.len() >= 32 {
                        return Err(SettingsError::Profile("profile limit reached".into()));
                    }
                    if let Some(source) = target {
                        self.resolve_profile(Some(source))?;
                        let source_root = self.profile_root(source)?;
                        let destination = self.profile_root(new_name)?;
                        fs::create_dir_all(&source_root)?;
                        fs::create_dir_all(&destination)?;
                        for entry in fs::read_dir(source_root)? {
                            let entry = entry?;
                            if entry
                                .file_name()
                                .to_string_lossy()
                                .ends_with(".settings.values.toml")
                            {
                                reject_symlink(&entry.path())?;
                                atomic_bytes(
                                    &destination.join(entry.file_name()),
                                    &fs::read(entry.path())?,
                                    true,
                                )?;
                            }
                        }
                    }
                    registry.profiles.push(new_name.into());
                } else {
                    if name == "default" {
                        return Err(SettingsError::Profile("default cannot be renamed".into()));
                    }
                    let index = registry
                        .profiles
                        .iter()
                        .position(|p| p == name)
                        .ok_or_else(|| SettingsError::Profile("unknown profile".into()))?;
                    let source = self.profile_root(name)?;
                    if source.exists() {
                        fs::rename(source, self.profile_root(new_name)?)?;
                    }
                    registry.profiles[index] = new_name.into();
                    if registry.active == name {
                        registry.active = new_name.into();
                    }
                }
            }
            "delete" => {
                self.resolve_profile(Some(name))?;
                if name == "default" {
                    return Err(SettingsError::Profile("default cannot be deleted".into()));
                }
                let root = self.profile_root(name)?;
                if root.exists() {
                    fs::remove_dir_all(root)?;
                }
                registry.profiles.retain(|p| p != name);
                if registry.active == name {
                    registry.active = "default".into();
                }
            }
            "activate" => {
                self.resolve_profile(Some(name))?;
                registry.active = name.into();
            }
            _ => return Err(SettingsError::Profile("unknown profile action".into())),
        }
        atomic_bytes(
            &self.settings_root.join("profiles.json"),
            &serde_json::to_vec(&registry)?,
            true,
        )?;
        Ok(registry)
    }

    pub fn copy_overlay(
        &self,
        name: &str,
        source: &str,
        target: &str,
    ) -> Result<(), SettingsError> {
        let _guard = PROFILE_MUTATIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.resolve_profile(Some(source))?;
        self.resolve_profile(Some(target))?;
        let (_, values) = self.load(&qualified_counter(name, source))?;
        let root = self.profile_root(target)?;
        fs::create_dir_all(&root)?;
        write_values_toml(&root.join(format!("{name}.settings.values.toml")), &values)?;
        Ok(())
    }

    pub fn load(&self, name: &str) -> Result<(Vec<CounterSetting>, CompactValues), SettingsError> {
        let (name, profile) = split_counter(name);
        validate_component(name)?;
        let profile = self.resolve_profile(profile)?;
        let counter_dir = self.require_counter_directory(name)?;
        fs::create_dir_all(&self.settings_root)?;
        let settings = self.load_or_migrate_schema(name, &counter_dir)?;
        let values = if profile == "default" {
            self.load_or_migrate_values(name, &counter_dir, &settings)?
        } else {
            let path = self
                .profile_root(&profile)?
                .join(format!("{name}.settings.values.toml"));
            if checked_exists(&path)? {
                read_values_toml(&path)?
            } else {
                defaults_from_schema(&settings)
            }
        };
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
        validate_component(split_counter(name).0)?;
        let (settings, mut values) = self.load(name)?;
        for item in payload {
            let sanitized_id = sanitize_id(&item.unique_id);
            let Some(setting) = settings
                .iter()
                .find(|setting| setting.unique_id == item.unique_id)
                .or_else(|| {
                    settings
                        .iter()
                        .find(|setting| setting.unique_id == sanitized_id)
                })
            else {
                continue;
            };
            let value = match setting.kind {
                SettingType::Number => coerce_number(item.value.clone()),
                SettingType::Checkbox => coerce_checkbox(item.value.clone()),
                _ => item.value.clone(),
            };
            values.insert(setting.unique_id.clone(), value);
        }
        Ok(values)
    }

    pub fn save_user_values(
        &self,
        name: &str,
        payload: &[SettingInput],
    ) -> Result<CompactValues, SettingsError> {
        let _guard = PROFILE_MUTATIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let values = self.prepare_user_values(name, payload)?;
        let (name, profile) = split_counter(name);
        let profile = self.resolve_profile(profile)?;
        let root = self.profile_root(&profile)?;
        fs::create_dir_all(&root)?;
        write_values_toml(&root.join(format!("{name}.settings.values.toml")), &values)
    }

    pub fn save_developer_settings(
        &self,
        name: &str,
        mut settings: Vec<CounterSetting>,
    ) -> Result<Vec<CounterSetting>, SettingsError> {
        let (name, profile) = split_counter(name);
        self.resolve_profile(profile)?;
        validate_component(name)?;
        self.require_counter_directory(name)?;
        for setting in &mut settings {
            setting.unique_id = sanitize_id(&setting.unique_id);
        }
        normalize_settings(&mut settings);
        write_schema_toml(&self.schema_path(name), &settings)?;
        Ok(settings)
    }

    pub fn schema_path(&self, name: &str) -> PathBuf {
        self.static_root.join(name).join("settings.schema.toml")
    }

    pub fn values_path(&self, name: &str) -> PathBuf {
        self.settings_root
            .join(format!("{name}.settings.values.toml"))
    }

    fn prior_xdg_schema_path(&self, name: &str) -> PathBuf {
        self.settings_root.join(format!("{name}.settings.toml"))
    }

    fn require_counter_directory(&self, name: &str) -> Result<PathBuf, SettingsError> {
        let path = self.static_root.join(name);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                SettingsError::MissingCounter
            } else {
                SettingsError::Io(error)
            }
        })?;
        if metadata.file_type().is_symlink() {
            let target = fs::metadata(&path).map_err(|_| SettingsError::UnsafeCounterPath)?;
            if !target.is_dir() {
                return Err(SettingsError::UnsafeCounterPath);
            }
            return Ok(path);
        }
        let root = fs::canonicalize(&self.static_root)?;
        let canonical = fs::canonicalize(&path)?;
        if !metadata.is_dir() || canonical.parent() != Some(root.as_path()) {
            return Err(SettingsError::UnsafeCounterPath);
        }
        Ok(canonical)
    }

    fn load_or_migrate_schema(
        &self,
        name: &str,
        counter_dir: &Path,
    ) -> Result<Vec<CounterSetting>, SettingsError> {
        let target = self.schema_path(name);
        if checked_exists(&target)? {
            let mut settings = read_schema_toml(&target)?;
            normalize_settings(&mut settings);
            return Ok(settings);
        }

        let root_json = counter_dir.join("settings.json");
        let root_toml = counter_dir.join("settings.toml");
        let prior_xdg_toml = self.prior_xdg_schema_path(name);
        let internal_json = self.settings_root.join(format!("{name}.settings.json"));

        let mut settings = if checked_exists(&root_json)? {
            read_json::<Vec<CounterSetting>>(&root_json)?
        } else if checked_exists(&root_toml)? {
            infer_schema_from_values_toml(read_toml(&root_toml)?)?
        } else if checked_exists(&prior_xdg_toml)? {
            read_schema_toml(&prior_xdg_toml)?
        } else if checked_exists(&internal_json)? {
            read_json::<Vec<CounterSetting>>(&internal_json)?
        } else {
            Vec::new()
        };

        normalize_settings(&mut settings);
        if !create_schema_toml(&target, &settings)? {
            let mut authoritative = read_schema_toml(&target)?;
            normalize_settings(&mut authoritative);
            return Ok(authoritative);
        }
        Ok(settings)
    }

    fn load_or_migrate_values(
        &self,
        name: &str,
        counter_dir: &Path,
        settings: &[CounterSetting],
    ) -> Result<CompactValues, SettingsError> {
        let target = self.values_path(name);
        if checked_exists(&target)? {
            return read_values_toml(&target);
        }

        let root_schema = self.schema_path(name);
        let root_toml = counter_dir.join("settings.toml");
        let internal_json = self.settings_root.join(format!("{name}.values.json"));
        let root_json = counter_dir.join("settings.values.json");

        let root_toml_state = source_state(&root_toml)?;
        let root_schema_state = source_state(&root_schema)?;
        let native_values = match (root_toml_state, root_schema_state) {
            (SourceState::Symlink, _) | (_, SourceState::Symlink) => {
                return Err(SettingsError::UnsafeSettingsFile);
            }
            (SourceState::Present, SourceState::Present) => {
                let document = read_toml(&root_toml)?;
                if is_schema_shaped(&document) {
                    None
                } else {
                    Some(decode_values_document(document)?)
                }
            }
            _ => None,
        };
        let values = if let Some(values) = native_values {
            values
        } else if checked_exists(&internal_json)? {
            read_json::<CompactValues>(&internal_json)?
        } else if checked_exists(&root_json)? {
            read_json::<CompactValues>(&root_json)?
        } else {
            defaults_from_schema(settings)
        };
        let (created, values) = create_values_toml(&target, &values)?;
        if !created {
            return read_values_toml(&target);
        }
        Ok(values)
    }
}

pub fn validate_component(name: &str) -> Result<(), SettingsError> {
    if name.eq_ignore_ascii_case("tools") {
        return Err(SettingsError::InvalidName);
    }
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

pub(crate) fn coerce_number(value: Value) -> Value {
    match value {
        Value::Number(_) => value,
        Value::String(text) => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::from(0)),
        Value::Bool(value) => Value::from(if value { 1 } else { 0 }),
        Value::Null => Value::Null,
        _ => Value::from(0),
    }
}

pub(crate) fn coerce_checkbox(value: Value) -> Value {
    match value {
        Value::String(text) => Value::Bool(text == "true"),
        other => other,
    }
}

pub(crate) fn normalize_settings(settings: &mut [CounterSetting]) {
    for setting in settings {
        setting.value = match setting.kind {
            SettingType::Number => coerce_number(setting.value.clone()),
            SettingType::Checkbox => coerce_checkbox(setting.value.clone()),
            _ => setting.value.clone(),
        };
    }
}

fn defaults_from_schema(settings: &[CounterSetting]) -> CompactValues {
    settings
        .iter()
        .filter(|setting| !setting.value.is_null())
        .map(|setting| (setting.unique_id.clone(), setting.value.clone()))
        .collect()
}

fn read_json<T>(path: &Path) -> Result<T, SettingsError>
where
    T: serde::de::DeserializeOwned,
{
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn read_toml(path: &Path) -> Result<toml::Value, SettingsError> {
    Ok(toml::from_slice(&fs::read(path)?)?)
}

fn read_schema_toml(path: &Path) -> Result<Vec<CounterSetting>, SettingsError> {
    decode_schema_document(read_toml(path)?)
}

fn decode_schema_document(document: toml::Value) -> Result<Vec<CounterSetting>, SettingsError> {
    if !is_schema_shaped(&document) {
        return Err(SettingsError::TomlConversion(
            "schema document must contain a top-level settings array of tables".to_owned(),
        ));
    }
    let json = toml_to_json(document, "settings schema")?;
    serde_json::from_value::<SchemaDocument>(json)
        .map(|document| document.settings)
        .map_err(|error| {
            SettingsError::TomlConversion(format!("invalid settings schema fields: {error}"))
        })
}

fn is_schema_shaped(document: &toml::Value) -> bool {
    let toml::Value::Table(table) = document else {
        return false;
    };
    matches!(
        table.get("settings"),
        Some(toml::Value::Array(settings))
            if settings
                .iter()
                .all(|setting| matches!(setting, toml::Value::Table(_)))
    )
}

fn infer_schema_from_values_toml(
    document: toml::Value,
) -> Result<Vec<CounterSetting>, SettingsError> {
    let toml::Value::Table(values) = document else {
        return Err(SettingsError::TomlConversion(
            "settings values must be a top-level TOML table".to_owned(),
        ));
    };

    values
        .into_iter()
        .map(|(unique_id, value)| {
            let kind = match &value {
                toml::Value::String(_) => SettingType::Text,
                toml::Value::Integer(_) | toml::Value::Float(_) => SettingType::Number,
                toml::Value::Boolean(_) => SettingType::Checkbox,
                toml::Value::Array(_) => {
                    return Err(SettingsError::TomlConversion(format!(
                        "cannot infer a setting type for array value {unique_id:?}"
                    )));
                }
                toml::Value::Table(_) => {
                    return Err(SettingsError::TomlConversion(format!(
                        "cannot infer a setting type for table value {unique_id:?}"
                    )));
                }
                toml::Value::Datetime(_) => {
                    return Err(SettingsError::TomlConversion(format!(
                        "cannot infer a setting type for datetime value {unique_id:?}"
                    )));
                }
            };
            let value = toml_to_json(value, &format!("settings value {unique_id:?}"))?;
            Ok(CounterSetting {
                title: unique_id.clone(),
                unique_id,
                unique_check: None,
                kind,
                options: None,
                description: String::new(),
                value,
            })
        })
        .collect()
}

fn read_values_toml(path: &Path) -> Result<CompactValues, SettingsError> {
    decode_values_document(read_toml(path)?)
}

fn decode_values_document(document: toml::Value) -> Result<CompactValues, SettingsError> {
    match toml_to_json(document, "settings values")? {
        Value::Object(values) => Ok(values),
        _ => Err(SettingsError::TomlConversion(
            "values document must be a top-level table".to_owned(),
        )),
    }
}

fn encode_schema_toml(settings: &[CounterSetting]) -> Result<String, SettingsError> {
    let mut settings = match serde_json::to_value(settings)? {
        Value::Array(settings) => settings,
        _ => {
            return Err(SettingsError::TomlConversion(
                "settings schema did not serialize to an array".to_owned(),
            ));
        }
    };
    for setting in &mut settings {
        let Value::Object(fields) = setting else {
            return Err(SettingsError::TomlConversion(
                "settings schema entry did not serialize to a table".to_owned(),
            ));
        };
        if fields.get("value").is_some_and(Value::is_null) {
            fields.remove("value");
        }
    }

    let mut document = toml::Table::new();
    document.insert(
        "settings".to_owned(),
        json_to_toml(&Value::Array(settings), "settings schema")?,
    );
    serialize_toml(toml::Value::Table(document))
}

fn write_schema_toml(path: &Path, settings: &[CounterSetting]) -> Result<(), SettingsError> {
    let encoded = encode_schema_toml(settings)?;
    atomic_bytes(path, encoded.as_bytes(), true)?;
    Ok(())
}

fn create_schema_toml(path: &Path, settings: &[CounterSetting]) -> Result<bool, SettingsError> {
    let encoded = encode_schema_toml(settings)?;
    atomic_bytes(path, encoded.as_bytes(), false)
}

fn encode_values_toml(values: &CompactValues) -> Result<(CompactValues, String), SettingsError> {
    let mut normalized = CompactValues::new();
    let mut document = toml::Table::new();
    for (id, value) in values {
        if value.is_null() {
            continue;
        }
        document.insert(id.clone(), json_to_toml(value, id)?);
        normalized.insert(id.clone(), value.clone());
    }
    let encoded = serialize_toml(toml::Value::Table(document))?;
    Ok((normalized, encoded))
}

fn write_values_toml(path: &Path, values: &CompactValues) -> Result<CompactValues, SettingsError> {
    let (normalized, encoded) = encode_values_toml(values)?;
    atomic_bytes(path, encoded.as_bytes(), true)?;
    Ok(normalized)
}

fn create_values_toml(
    path: &Path,
    values: &CompactValues,
) -> Result<(bool, CompactValues), SettingsError> {
    let (normalized, encoded) = encode_values_toml(values)?;
    let created = atomic_bytes(path, encoded.as_bytes(), false)?;
    Ok((created, normalized))
}

fn serialize_toml(document: toml::Value) -> Result<String, SettingsError> {
    let mut encoded = toml::to_string_pretty(&document)?;
    if !encoded.ends_with('\n') {
        encoded.push('\n');
    }
    Ok(encoded)
}

fn json_to_toml(value: &Value, context: &str) -> Result<toml::Value, SettingsError> {
    match value {
        Value::Null => Err(SettingsError::TomlConversion(format!(
            "{context:?} contains null, which TOML cannot represent"
        ))),
        Value::Bool(value) => Ok(toml::Value::Boolean(*value)),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(toml::Value::Integer(value))
            } else if let Some(value) = value.as_u64() {
                i64::try_from(value).map(toml::Value::Integer).map_err(|_| {
                    SettingsError::TomlConversion(format!(
                        "{context:?} contains integer {value}, outside TOML's signed 64-bit range"
                    ))
                })
            } else if let Some(value) = value.as_f64() {
                Ok(toml::Value::Float(value))
            } else {
                Err(SettingsError::TomlConversion(format!(
                    "{context:?} contains an unsupported JSON number"
                )))
            }
        }
        Value::String(value) => Ok(toml::Value::String(value.clone())),
        Value::Array(values) => values
            .iter()
            .map(|value| json_to_toml(value, context))
            .collect::<Result<Vec<_>, _>>()
            .map(toml::Value::Array),
        Value::Object(values) => {
            let mut table = toml::Table::new();
            for (key, value) in values {
                table.insert(key.clone(), json_to_toml(value, context)?);
            }
            Ok(toml::Value::Table(table))
        }
    }
}

fn toml_to_json(value: toml::Value, context: &str) -> Result<Value, SettingsError> {
    match value {
        toml::Value::String(value) => Ok(Value::String(value)),
        toml::Value::Integer(value) => Ok(Value::from(value)),
        toml::Value::Float(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| {
                SettingsError::TomlConversion(format!(
                    "{context:?} contains non-finite float {value}"
                ))
            }),
        toml::Value::Boolean(value) => Ok(Value::Bool(value)),
        toml::Value::Datetime(value) => Err(SettingsError::TomlConversion(format!(
            "{context:?} contains TOML datetime {value}, which JSON settings cannot represent"
        ))),
        toml::Value::Array(values) => values
            .into_iter()
            .map(|value| toml_to_json(value, context))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        toml::Value::Table(values) => {
            let mut object = serde_json::Map::new();
            for (key, value) in values {
                object.insert(key, toml_to_json(value, context)?);
            }
            Ok(Value::Object(object))
        }
    }
}

fn source_state(path: &Path) -> Result<SourceState, SettingsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(SourceState::Symlink),
        Ok(_) => Ok(SourceState::Present),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SourceState::Missing),
        Err(error) => Err(error.into()),
    }
}

fn checked_exists(path: &Path) -> Result<bool, SettingsError> {
    match source_state(path)? {
        SourceState::Missing => Ok(false),
        SourceState::Present => Ok(true),
        SourceState::Symlink => Err(SettingsError::UnsafeSettingsFile),
    }
}

fn reject_symlink(path: &Path) -> Result<(), SettingsError> {
    checked_exists(path).map(|_| ())
}

#[cfg(not(windows))]
fn persist_noclobber(temp: tempfile::NamedTempFile, path: &Path) -> Result<bool, SettingsError> {
    match temp.persist_noclobber(path) {
        Ok(file) => {
            drop(file);
            Ok(true)
        }
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error.into()),
    }
}

#[cfg(windows)]
fn persist_noclobber(temp: tempfile::NamedTempFile, path: &Path) -> Result<bool, SettingsError> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let (file, temp_path) = temp.keep().map_err(|error| error.error)?;
    drop(file);
    let temp_wide = temp_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            temp_wide.as_ptr(),
            path_wide.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved != 0 {
        return Ok(true);
    }

    let error = io::Error::last_os_error();
    let _ = fs::remove_file(&temp_path);
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS) => Ok(false),
        _ => Err(error.into()),
    }
}

fn atomic_bytes(path: &Path, value: &[u8], replace_existing: bool) -> Result<bool, SettingsError> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "settings path has no parent")
    })?;
    reject_symlink(path)?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".settings-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    temp.write_all(value)?;
    temp.flush()?;
    temp.as_file().sync_all()?;

    if !replace_existing {
        let created = persist_noclobber(temp, path)?;
        if !created {
            // A developer write or another migration won the race. Treat its
            // complete file as authoritative and never replace it.
            reject_symlink(path)?;
            return Ok(false);
        }
        sync_parent(parent)?;
        return Ok(true);
    }

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
    Ok(true)
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}
#[cfg(not(unix))]
fn sync_parent(_: &Path) -> io::Result<()> {
    Ok(())
}
