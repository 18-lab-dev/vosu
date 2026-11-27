//! One-time import: the source is parsed, never initialized or watched.
use super::document::ConfigDocument;
use super::io::{AtomicReplace, atomic_write, config_parent, reject_target_symlink};
use super::migration::{
    InitialConfig, initialize_file, legacy_table, parse_env, read_authoritative,
};
use super::parser::parse_toml;
use std::{fs, io, path::Path};

pub(super) fn initialize(
    path: &Path,
    source: &Path,
    replacer: &dyn AtomicReplace,
) -> io::Result<InitialConfig> {
    reject_target_symlink(path)?;
    let parent = config_parent(path);
    // Check alpha inputs first, without touching the normal tree. Even an
    // invalid existing alpha config must not silently fall back to 1.x.
    // The target may be inside Windows' bounded replacement-name gap.
    // Use the published-file reader before deciding this is a first start.
    if read_authoritative(path)?.is_some() {
        return initialize_file(path, replacer);
    }
    for local in [parent.join("vosu.env"), parent.join("tosu.env")] {
        match fs::symlink_metadata(local) {
            Ok(_) => return initialize_file(path, replacer),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    let source_parent = config_parent(source);
    // read_authoritative uses the Windows-safe published-file reader. A
    // missing source is fine; unreadable or malformed input fails before any
    // target publication, preserving all source bytes and allowing a retry.
    let toml = read_authoritative(source)?;
    let mut document = ConfigDocument::default();
    for legacy in [
        source_parent.join("tosu.env"),
        source_parent.join("vosu.env"),
    ] {
        match fs::read_to_string(legacy) {
            Ok(content) => document.apply_table(&legacy_table(&parse_env(&content))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if let Some(content) = toml {
        let table = parse_toml(&content)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        document.apply_table(&table);
    }
    let warnings = document.validate();
    let bytes = document.serialize().into_bytes();
    fs::create_dir_all(parent)?;
    atomic_write(path, &bytes, replacer)?;
    Ok(InitialConfig {
        config: document.into_config(),
        bytes,
        warnings,
    })
}

#[cfg(test)]
mod tests;
