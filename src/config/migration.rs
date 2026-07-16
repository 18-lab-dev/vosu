use super::document::ConfigDocument;
use super::io::{AtomicReplace, atomic_write};
use super::parser::{env_binding_order, parse_env};
use super::schema::{BINDING_ORDER, Config, DEFAULT_SCHEMA, NEWLINE_INSERTIONS};
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
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    // `tosu.env` is a one-time input from installations predating the vosu rename.
    let old_path = parent.join("tosu.env");
    let current_content = std::fs::read_to_string(path).ok();
    let old_content = std::fs::read_to_string(&old_path).ok();
    let current_env = current_content
        .as_deref()
        .map(parse_env)
        .unwrap_or_default();
    let old_env = old_content.as_deref().map(parse_env).unwrap_or_default();
    let known = DEFAULT_SCHEMA
        .items()
        .iter()
        .map(|item| item.binding)
        .collect::<std::collections::HashSet<_>>();
    let deprecated_order = env_binding_order(if old_content.is_some() {
        old_content.as_deref().unwrap_or_default()
    } else {
        current_content.as_deref().unwrap_or_default()
    });
    let deprecated = deprecated_order
        .into_iter()
        .filter(|key| !known.contains(key.as_str()))
        .collect::<Vec<_>>();
    let new_properties = DEFAULT_SCHEMA
        .items()
        .iter()
        .filter(|item| !current_env.contains_key(item.binding))
        .map(|item| item.binding)
        .collect::<Vec<_>>();

    let defaults = ConfigDocument::default();
    let mut output = String::new();
    for (index, binding) in BINDING_ORDER.iter().enumerate() {
        if index != 0 {
            output.push('\n');
        }
        output.push_str(binding);
        output.push('=');
        let value = current_env
            .get(*binding)
            .or_else(|| old_env.get(*binding))
            .cloned()
            .unwrap_or_else(|| defaults.value_for_binding(binding));
        output.push_str(&value);
        if NEWLINE_INSERTIONS.contains(binding) {
            output.push('\n');
        }
    }
    let bytes = output.into_bytes();
    atomic_write(path, &bytes, replacer)?;
    if old_content.is_some() {
        match std::fs::remove_file(&old_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

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
    if old_content.is_some() || !deprecated.is_empty() || !new_properties.is_empty() {
        warnings.push("Configuration file successfully migrated.".to_owned());
    }
    let mut document = ConfigDocument::default();
    warnings.extend(document.refresh(std::str::from_utf8(&bytes).expect("ASCII config")));
    Ok(InitialConfig {
        config: document.into_config(),
        bytes,
        warnings,
    })
}
