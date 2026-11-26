//! The overlay root setting (A7.1-OR): `vosu.overlayhost` learns its root
//! only as the `static_folder_path` ABI setting (plugin-abi.md §6.9), which
//! core seeds from `STATIC_FOLDER_PATH`, already resolved to an absolute
//! path by the unchanged rule, until M8 migrates the key. No pathname ever
//! crosses `vosu-surface`.

use std::fmt;
use std::path::PathBuf;

use vosu_plugin_api::CborValue;

/// The one `settings.schema.json` key.
pub const STATIC_FOLDER_PATH: &str = "static_folder_path";

/// Why a settings object does not name a usable overlay root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootSettingError {
    /// The settings value is not a map.
    NotAMap,
    /// `static_folder_path` is not a string.
    TypeMismatch,
    /// `static_folder_path` is not an absolute path: core resolves the
    /// configured spelling before delivery, so a relative value is a
    /// delivery defect, never resolved against this process's directory.
    NotAbsolute,
}

impl fmt::Display for RootSettingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotAMap => "overlay host settings are not a map",
            Self::TypeMismatch => "static_folder_path is not a string",
            Self::NotAbsolute => "static_folder_path is not an absolute path",
        })
    }
}

impl std::error::Error for RootSettingError {}

/// Decodes the complete effective settings object (`Init::settings` or a
/// `SETTINGS_CHANGED` payload). `Ok(None)` is the schema default `""`: no
/// root is configured yet, so nothing is served or written.
pub fn from_cbor(settings: &CborValue) -> Result<Option<PathBuf>, RootSettingError> {
    let CborValue::Map(entries) = settings else {
        return Err(RootSettingError::NotAMap);
    };
    let value = entries.iter().find_map(|(key, value)| match key {
        CborValue::Text(key) if key == STATIC_FOLDER_PATH => Some(value),
        _ => None,
    });
    match value {
        None => Ok(None),
        Some(CborValue::Text(path)) => parse(path),
        Some(_) => Err(RootSettingError::TypeMismatch),
    }
}

/// [`from_cbor`] for the same object in its JSON spelling.
pub fn from_json(settings: &serde_json::Value) -> Result<Option<PathBuf>, RootSettingError> {
    let serde_json::Value::Object(entries) = settings else {
        return Err(RootSettingError::NotAMap);
    };
    match entries.get(STATIC_FOLDER_PATH) {
        None => Ok(None),
        Some(serde_json::Value::String(path)) => parse(path),
        Some(_) => Err(RootSettingError::TypeMismatch),
    }
}

/// The complete effective settings object for `root`, as core delivers it.
pub fn settings_object(root: &std::path::Path) -> serde_json::Value {
    serde_json::json!({ STATIC_FOLDER_PATH: root.to_string_lossy() })
}

fn parse(path: &str) -> Result<Option<PathBuf>, RootSettingError> {
    if path.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(RootSettingError::NotAbsolute);
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(value: CborValue) -> CborValue {
        CborValue::Map(vec![(CborValue::Text(STATIC_FOLDER_PATH.into()), value)])
    }

    #[test]
    fn only_an_absolute_string_names_a_root() {
        let absolute = std::env::temp_dir().join("overlays");
        assert_eq!(
            from_cbor(&map(CborValue::Text(absolute.to_string_lossy().into()))),
            Ok(Some(absolute.clone()))
        );
        assert_eq!(from_json(&settings_object(&absolute)), Ok(Some(absolute)));
        assert_eq!(from_cbor(&map(CborValue::Text(String::new()))), Ok(None));
        assert_eq!(from_cbor(&CborValue::Map(Vec::new())), Ok(None));
        assert_eq!(
            from_cbor(&map(CborValue::Text("./static".into()))),
            Err(RootSettingError::NotAbsolute)
        );
        assert_eq!(
            from_cbor(&map(CborValue::Bool(true))),
            Err(RootSettingError::TypeMismatch)
        );
        assert_eq!(
            from_cbor(&CborValue::Bool(true)),
            Err(RootSettingError::NotAMap)
        );
        assert_eq!(
            from_json(&serde_json::json!({ STATIC_FOLDER_PATH: "../static" })),
            Err(RootSettingError::NotAbsolute)
        );
    }

    #[test]
    fn bundled_schema_declares_exactly_the_root_text_setting() {
        let schema =
            vosu_plugin_api::manifest::settings::SettingsSchema::parse(crate::SETTINGS_SCHEMA)
                .unwrap();
        let entries: serde_json::Value = serde_json::from_str(crate::SETTINGS_SCHEMA).unwrap();
        assert_eq!(entries.as_array().map(Vec::len), Some(1));
        assert_eq!(entries[0]["uniqueID"], STATIC_FOLDER_PATH);
        assert_eq!(entries[0]["type"], "text");
        drop(schema);
    }
}
