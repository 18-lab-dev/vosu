use super::document::ConfigDocument;
use super::io::{AtomicReplace, atomic_write, config_parent, reject_target_symlink};
use super::parser::parse_toml;
use super::schema::{Config, ConfigDefault, DEFAULT_SCHEMA};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

pub(super) struct InitialConfig {
    pub(super) config: Config,
    pub(super) bytes: Vec<u8>,
    pub(super) warnings: Vec<String>,
}

pub(super) fn initialize_file(
    path: &Path,
    replacer: &dyn AtomicReplace,
) -> io::Result<InitialConfig> {
    reject_target_symlink(path)?;
    let parent = config_parent(path);
    fs::create_dir_all(parent)?;

    let vosu_env_path = parent.join("vosu.env");
    let tosu_env_path = parent.join("tosu.env");
    let toml_content = read_authoritative(path)?;
    let vosu_env_content = fs::read_to_string(&vosu_env_path).ok();
    let tosu_env_content = fs::read_to_string(&tosu_env_path).ok();

    // Parse the authoritative source before creating, replacing, or removing
    // anything. In particular, malformed TOML must leave every source intact.
    let toml_table = toml_content
        .as_deref()
        .map(parse_toml)
        .transpose()
        .map_err(invalid_toml)?;
    let vosu_env = vosu_env_content.as_deref().map(parse_env);
    let tosu_env = tosu_env_content.as_deref().map(parse_env);

    let mut document = ConfigDocument::default();
    if let Some(values) = tosu_env.as_ref() {
        document.apply_table(&legacy_table(values));
    }
    if let Some(values) = vosu_env.as_ref() {
        document.apply_table(&legacy_table(values));
    }
    if let Some(values) = toml_table.as_ref() {
        document.apply_table(values);
    }
    let clamp_warnings = document.validate();
    let bytes = document.serialize().into_bytes();

    let mut deprecated = Vec::new();
    for content in [tosu_env_content.as_deref(), vosu_env_content.as_deref()]
        .into_iter()
        .flatten()
    {
        for binding in env_binding_order(content) {
            if !is_known(&binding) && !deprecated.contains(&binding) {
                deprecated.push(binding);
            }
        }
    }
    if let Some(values) = toml_table.as_ref() {
        for binding in values.keys() {
            if !is_known(binding) && !deprecated.contains(binding) {
                deprecated.push(binding.clone());
            }
        }
    }

    // Preserve the old migration diagnostic: bindings absent from the current
    // source (authoritative TOML, or vosu.env before TOML exists) are additions.
    let new_properties = DEFAULT_SCHEMA
        .items()
        .iter()
        .filter(|item| {
            if let Some(values) = toml_table.as_ref() {
                !values.contains_key(item.binding)
            } else if let Some(values) = vosu_env.as_ref() {
                !values.contains_key(item.binding)
            } else {
                true
            }
        })
        .map(|item| item.binding)
        .collect::<Vec<_>>();
    let canonical_was_current = toml_content
        .as_deref()
        .is_some_and(|content| content.as_bytes() == bytes.as_slice());

    // Canonical TOML must be durable before any readable migration input is
    // removed. AtomicReplace guarantees the target is untouched on failure.
    atomic_write(path, &bytes, replacer)?;
    remove_readable_legacy(
        path,
        parent,
        [
            (&vosu_env_path, vosu_env_content.is_some()),
            (&tosu_env_path, tosu_env_content.is_some()),
        ],
    )?;

    let mut warnings = Vec::new();
    if !deprecated.is_empty() {
        warnings.push(format!(
            "Removed deprecated configuration properties: %{}%",
            deprecated.join(", ")
        ));
    }
    if !new_properties.is_empty() {
        warnings.push(format!(
            "Added new configuration properties: %{}%",
            new_properties.join(", ")
        ));
    }
    if !canonical_was_current
        || vosu_env_content.is_some()
        || tosu_env_content.is_some()
        || !deprecated.is_empty()
        || !new_properties.is_empty()
    {
        warnings.push("Configuration file successfully migrated.".to_owned());
    }
    warnings.extend(clamp_warnings);

    Ok(InitialConfig {
        config: document.into_config(),
        bytes,
        warnings,
    })
}

fn read_authoritative(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn invalid_toml(error: toml::de::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn is_known(binding: &str) -> bool {
    DEFAULT_SCHEMA
        .items()
        .iter()
        .any(|item| item.binding == binding)
}

fn legacy_table(values: &HashMap<String, String>) -> toml::Table {
    let mut table = toml::Table::new();
    for item in DEFAULT_SCHEMA.items() {
        let Some(raw) = values.get(item.binding) else {
            continue;
        };
        let value = match item.default {
            ConfigDefault::Boolean(_) => toml::Value::Boolean(raw == "true"),
            ConfigDefault::Number(default) => {
                toml::Value::Float(parse_real_number(raw).unwrap_or(default))
            }
            ConfigDefault::String(_) => toml::Value::String(raw.clone()),
        };
        table.insert(item.binding.to_owned(), value);
    }
    table
}

fn remove_readable_legacy(
    target: &Path,
    parent: &Path,
    sources: [(&Path, bool); 2],
) -> io::Result<()> {
    let mut removed = false;
    let mut removal_error = None;
    for (source, readable) in sources {
        if !readable || source == target {
            continue;
        }
        match fs::remove_file(source) {
            Ok(()) => removed = true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                removal_error = Some(error);
                break;
            }
        }
    }
    if removed {
        sync_parent(parent)?;
    }
    if let Some(error) = removal_error {
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_: &Path) -> io::Result<()> {
    Ok(())
}

fn env_binding_order(content: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for original_line in content.lines() {
        let mut line = original_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        line = strip_export(line);
        let Some((key, _)) = split_env_binding(line) else {
            continue;
        };
        let key = key.trim();
        if !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
            && !keys.iter().any(|existing| existing == key)
        {
            keys.push(key.to_owned());
        }
    }
    keys
}
fn strip_export(line: &str) -> &str {
    let Some(rest) = line.strip_prefix("export") else {
        return line;
    };
    if rest.starts_with(char::is_whitespace) {
        rest.trim_start()
    } else {
        line
    }
}

fn parse_real_number(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let prefixed_integer = [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ]
    .into_iter()
    .find_map(|(prefix, radix)| {
        raw.strip_prefix(prefix)
            .and_then(|digits| u64::from_str_radix(digits, radix).ok())
    });

    prefixed_integer
        .map(|value| value as f64)
        .or_else(|| raw.parse::<f64>().ok())
        .filter(|value| value.is_finite())
}

/// Parses the dotenv forms used by the legacy source files. Later duplicate
/// bindings replace earlier ones, matching `dotenv.parse`.
fn parse_env(content: &str) -> HashMap<String, String> {
    let mut parsed = HashMap::new();

    for original_line in content.lines() {
        let mut line = original_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        line = strip_export(line);

        let Some((key, value)) = split_env_binding(line) else {
            continue;
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            continue;
        }

        parsed.insert(key.to_owned(), parse_env_value(value));
    }

    parsed
}

fn split_env_binding(line: &str) -> Option<(&str, &str)> {
    if let Some(binding) = line.split_once('=') {
        return Some(binding);
    }

    let (key, value) = line.split_once(':')?;
    value
        .starts_with(char::is_whitespace)
        .then_some((key, value.trim_start()))
}

fn parse_env_value(raw: &str) -> String {
    let value = raw.trim();
    let Some(quote) = value
        .as_bytes()
        .first()
        .copied()
        .filter(|quote| matches!(quote, b'\'' | b'"' | b'`'))
    else {
        return value
            .split_once('#')
            .map_or(value, |(before_comment, _)| before_comment)
            .trim_end()
            .to_owned();
    };

    let mut escaped = false;
    let closing_quote = value.as_bytes()[1..]
        .iter()
        .position(|byte| {
            if escaped {
                escaped = false;
                return false;
            }
            if *byte == b'\\' {
                escaped = true;
                return false;
            }
            *byte == quote
        })
        .map(|position| position + 1);

    let Some(closing_quote) = closing_quote else {
        return value.to_owned();
    };
    let inner = &value[1..closing_quote];
    if quote == b'"' {
        inner.replace("\\n", "\n").replace("\\r", "\r")
    } else {
        inner.to_owned()
    }
}
