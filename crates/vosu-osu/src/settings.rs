//! `OsuSettings` (§6.9, D19): the five osu!-specific keys this plugin's
//! `settings.schema.json` declares, and `Init`'s/`SETTINGS_CHANGED`'s
//! `CborValue` payload decoded against it. Defaults mirror
//! `vosu_osu_state::ReaderSettings::default()` for these five fields, so
//! an unset setting and core's old bridge-B2 default agree
//! (`crates/vosu-core/src/plugin/settings.rs`'s `ConfigSettingsBridgeB2::schema`,
//! deleted in unit P0).

use std::fmt;

use vosu_plugin_api::CborValue;

/// The plugin's effective settings, decoded from a CBOR map (`Init::settings`
/// or a `VOSU_OP_SETTINGS_CHANGED` payload).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OsuSettings {
    pub calculate_pp: bool,
    pub poll_rate: f64,
    pub precise_data_poll_rate: f64,
    pub show_mp_commands: bool,
    pub read_mania_scroll_speed: bool,
}

impl Default for OsuSettings {
    fn default() -> Self {
        Self {
            calculate_pp: true,
            poll_rate: 150.0,
            precise_data_poll_rate: 10.0,
            show_mp_commands: false,
            read_mania_scroll_speed: true,
        }
    }
}

/// Every way a settings `CborValue` fails to decode against the five
/// declared keys.
#[derive(Debug, Clone, PartialEq)]
pub enum OsuSettingsError {
    /// The top-level value is not a CBOR map.
    NotAMap,
    /// A known key's value is not the CBOR type its schema entry declares
    /// (`checkbox` -> bool, `number` -> integer or float).
    TypeMismatch(&'static str),
}

impl fmt::Display for OsuSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAMap => write!(f, "settings value is not a CBOR map"),
            Self::TypeMismatch(key) => write!(f, "setting {key:?}'s value has the wrong type"),
        }
    }
}

impl std::error::Error for OsuSettingsError {}

fn as_bool(value: &CborValue, key: &'static str) -> Result<bool, OsuSettingsError> {
    match value {
        CborValue::Bool(b) => Ok(*b),
        _ => Err(OsuSettingsError::TypeMismatch(key)),
    }
}

fn as_number(value: &CborValue, key: &'static str) -> Result<f64, OsuSettingsError> {
    match value {
        CborValue::Float(n) => Ok(*n),
        CborValue::Integer(i) => i128::from(*i)
            .try_into()
            .map(|i: i64| i as f64)
            .map_err(|_| OsuSettingsError::TypeMismatch(key)),
        _ => Err(OsuSettingsError::TypeMismatch(key)),
    }
}

impl OsuSettings {
    /// Decodes `value` (a CBOR map) into effective settings: every known
    /// key overrides [`OsuSettings::default`]; an unrecognized map key is
    /// ignored (the host validates the schema before this ever runs,
    /// §6.9 "core owns storage").
    pub fn from_cbor(value: &CborValue) -> Result<Self, OsuSettingsError> {
        let CborValue::Map(entries) = value else {
            return Err(OsuSettingsError::NotAMap);
        };
        let mut settings = Self::default();
        for (key, value) in entries {
            let CborValue::Text(key) = key else {
                continue;
            };
            match key.as_str() {
                "CALCULATE_PP" => settings.calculate_pp = as_bool(value, "CALCULATE_PP")?,
                "POLL_RATE" => settings.poll_rate = as_number(value, "POLL_RATE")?,
                "PRECISE_DATA_POLL_RATE" => {
                    settings.precise_data_poll_rate = as_number(value, "PRECISE_DATA_POLL_RATE")?;
                }
                "SHOW_MP_COMMANDS" => {
                    settings.show_mp_commands = as_bool(value, "SHOW_MP_COMMANDS")?;
                }
                "READ_MANIA_SCROLL_SPEED" => {
                    settings.read_mania_scroll_speed = as_bool(value, "READ_MANIA_SCROLL_SPEED")?;
                }
                _ => {}
            }
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: Vec<(&str, CborValue)>) -> CborValue {
        CborValue::Map(
            entries
                .into_iter()
                .map(|(k, v)| (CborValue::Text(k.into()), v))
                .collect(),
        )
    }

    #[test]
    fn default_decodes_from_an_empty_map() {
        assert_eq!(
            OsuSettings::from_cbor(&map(vec![])).unwrap(),
            OsuSettings::default()
        );
    }

    #[test]
    fn overrides_every_known_key() {
        let value = map(vec![
            ("CALCULATE_PP", CborValue::Bool(false)),
            ("POLL_RATE", CborValue::Float(250.0)),
            ("PRECISE_DATA_POLL_RATE", CborValue::Float(5.0)),
            ("SHOW_MP_COMMANDS", CborValue::Bool(true)),
            ("READ_MANIA_SCROLL_SPEED", CborValue::Bool(false)),
        ]);
        let settings = OsuSettings::from_cbor(&value).unwrap();
        assert_eq!(
            settings,
            OsuSettings {
                calculate_pp: false,
                poll_rate: 250.0,
                precise_data_poll_rate: 5.0,
                show_mp_commands: true,
                read_mania_scroll_speed: false,
            }
        );
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let value = map(vec![("SOME_FUTURE_KEY", CborValue::Bool(true))]);
        assert_eq!(
            OsuSettings::from_cbor(&value).unwrap(),
            OsuSettings::default()
        );
    }

    #[test]
    fn integer_poll_rate_is_accepted() {
        let value = map(vec![("POLL_RATE", CborValue::Integer(200.into()))]);
        assert_eq!(OsuSettings::from_cbor(&value).unwrap().poll_rate, 200.0);
    }

    #[test]
    fn wrong_type_is_rejected() {
        let value = map(vec![("CALCULATE_PP", CborValue::Text("yes".into()))]);
        assert_eq!(
            OsuSettings::from_cbor(&value).unwrap_err(),
            OsuSettingsError::TypeMismatch("CALCULATE_PP")
        );
    }

    #[test]
    fn not_a_map_is_rejected() {
        assert_eq!(
            OsuSettings::from_cbor(&CborValue::Null).unwrap_err(),
            OsuSettingsError::NotAMap
        );
    }
}
