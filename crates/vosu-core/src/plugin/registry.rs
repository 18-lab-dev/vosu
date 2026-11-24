//! Plugin registry: loads the root crate's explicit `BUNDLED` array (D13)
//! and `--plugin <dir>` external `dlopen` plugins (`libloading`, D13), with
//! ABI version negotiation and descriptor checks (`docs/v2/plugin-abi.md`
//! §1, §2) and the §7.1 in-process trust decision.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::{Library, Symbol};
use vosu_plugin_api::{
    PluginManifest, Runtime, VOSU_PLUGIN_ABI_MAJOR, VosuPluginEntryFn, vosu_plugin_vtable,
};

use super::error::HostError;
use super::settings::SettingsSchema;

/// One bundled plugin entry the root crate's explicit `BUNDLED` array lists
/// (D13): its embedded manifest text (never carrying `[targets.*]`, §8) and
/// the plain vtable-builder function `export_plugin!(bundled T)` generates.
#[derive(Clone, Copy)]
pub struct BundledPlugin<'a> {
    pub manifest: &'a str,
    /// This plugin's own embedded `settings.schema.json` text (§6.9,
    /// §8's `settings_schema` manifest field), the same way `manifest`
    /// is embedded: `None` for a plugin that declares no
    /// `settings_schema`.
    pub settings_schema: Option<&'a str>,
    /// Embedded schemas for this plugin's namespace capabilities.
    pub namespace_schemas: &'a [&'a str],
    /// Package-relative paths paired with embedded action schema text.
    pub action_schemas: &'a [(&'a str, &'a str)],
    pub vtable: fn() -> vosu_plugin_vtable,
}

/// A manifest-validated, ABI-checked plugin ready for
/// `super::instance::PluginInstance::create`.
pub struct LoadedPlugin {
    manifest: PluginManifest,
    vtable: vosu_plugin_vtable,
    /// This plugin's own parsed `settings.schema.json` (§6.9), if it
    /// declares one: `create_instance` builds `init.settings` against it
    /// and keeps a copy for bridge-style settings-change watchers (§8's
    /// `settings_schema` manifest field; `BundledPlugin::settings_schema`/
    /// `load_development`'s `manifest.settings_schema` path).
    settings_schema: Option<SettingsSchema>,
    namespace_schemas: Vec<String>,
    action_schemas: std::collections::BTreeMap<String, String>,
    compiled_action_schemas:
        std::collections::BTreeMap<String, Arc<crate::action_schema::CompiledActionSchema>>,
    // Shared, not just held: unloading while an instance's `plugin_ctx`
    // still points into the library would be immediate undefined behavior
    // the next `dispatch`/`destroy` call makes, and `LoadedPlugin` alone
    // does not outlive the instances built from it (a caller is free to
    // drop it right after `PluginInstance::create`). `PluginInstance`
    // clones this `Arc` via `library_handle` so the library stays mapped
    // for as long as either side needs it. `None` for a bundled plugin,
    // which is linked into this executable rather than `dlopen`ed.
    library: Option<Arc<Library>>,
    /// Namespace-schema registration authority, decided by the loader from
    /// provenance: `TrustedOfficial` only for the compiled `BUNDLED` array
    /// and catalog packages installed from an exact official source identity
    /// (the same rule catalog I0 compiles staged schemas with). Debug
    /// `--plugin` overrides and trusted/user-source packages stay
    /// `Untrusted`; a dlopened library never implies official authority.
    schema_provenance: vosu_plugin_api::manifest::schema::SchemaProvenance,
}

impl LoadedPlugin {
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn vtable(&self) -> vosu_plugin_vtable {
        self.vtable
    }

    /// This plugin's own parsed `settings.schema.json`, if it declared
    /// one.
    pub fn settings_schema(&self) -> Option<&SettingsSchema> {
        self.settings_schema.as_ref()
    }

    pub fn namespace_schemas(&self) -> &[String] {
        &self.namespace_schemas
    }

    /// Raw schema assets keyed by exact manifest package path, for compilation.
    pub fn action_schemas(&self) -> &std::collections::BTreeMap<String, String> {
        &self.action_schemas
    }

    /// Compiled once during load, keyed by the exact manifest asset path.
    pub fn compiled_action_schemas(
        &self,
    ) -> &std::collections::BTreeMap<String, Arc<crate::action_schema::CompiledActionSchema>> {
        &self.compiled_action_schemas
    }

    /// Clones this plugin's `Arc<Library>` handle, so a `PluginInstance`
    /// can co-own it for its whole lifetime, independent of how long this
    /// `LoadedPlugin` itself stays alive. `None` for a bundled plugin.
    pub(crate) fn library_handle(&self) -> Option<Arc<Library>> {
        self.library.clone()
    }

    /// Provenance the state registry compiles this plugin's namespace
    /// schemas with (`x-visibility` requires `TrustedOfficial`).
    pub fn schema_provenance(&self) -> vosu_plugin_api::manifest::schema::SchemaProvenance {
        self.schema_provenance
    }
}

/// The minimum `struct_size` this major ABI version accepts: large enough
/// to hold the three required functions `create`, `dispatch`, `destroy`
/// (§1). Only one minor revision exists so far, so this equals the full
/// Rust struct size; a future additive minor revision keeps this constant
/// at the size this host understands, never the newer producer's size.
pub fn descriptor_minimum() -> usize {
    core::mem::size_of::<vosu_plugin_vtable>()
}

fn check_header(struct_size: u32, abi_version: u32) -> Result<(), HostError> {
    if abi_version != VOSU_PLUGIN_ABI_MAJOR {
        return Err(HostError::AbiMismatch {
            expected: VOSU_PLUGIN_ABI_MAJOR,
            found: abi_version,
        });
    }
    let minimum = descriptor_minimum();
    if (struct_size as usize) < minimum {
        return Err(HostError::DescriptorTooSmall {
            minimum,
            found: struct_size as usize,
        });
    }
    Ok(())
}

fn check_descriptor(vtable: &vosu_plugin_vtable) -> Result<(), HostError> {
    check_header(vtable.struct_size, vtable.abi_version)?;
    if vtable.create.is_none() || vtable.dispatch.is_none() || vtable.destroy.is_none() {
        return Err(HostError::MissingRequiredFunction);
    }
    Ok(())
}

/// The running host's target triple, matching `[targets.<triple>]` keys
/// (§8). Only the two released targets exist today.
fn current_target_triple() -> &'static str {
    if cfg!(all(target_arch = "x86_64", target_os = "windows")) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

/// Loads every bundled plugin. §7.1 grants `in-process` unconditionally to
/// the explicit `BUNDLED` array — it is never `dlopen`ed, so there is no
/// separate trust question to ask of it the way an external plugin's
/// `runtime` request raises one.
pub fn load_bundled(
    application_version: &semver::Version,
    snapshot: &super::host::ValidationSnapshot,
    bundled: &[BundledPlugin<'_>],
) -> Result<Vec<LoadedPlugin>, HostError> {
    bundled
        .iter()
        .map(|entry| {
            let manifest = parse_plugin(
                entry.manifest,
                application_version,
                snapshot,
                "bundled",
                true,
            )?;
            if manifest.runtime != Runtime::InProcess {
                return Err(HostError::InProcessRefused {
                    reason: "a bundled plugin's manifest must request runtime = \"in-process\"",
                });
            }
            if manifest.settings_schema.is_some() != entry.settings_schema.is_some() {
                return Err(HostError::SettingsSchemaMismatch {
                    plugin_id: manifest.id.clone(),
                    manifest_declares_one: manifest.settings_schema.is_some(),
                    entry_declares_one: entry.settings_schema.is_some(),
                });
            }
            let settings_schema = entry
                .settings_schema
                .map(SettingsSchema::parse)
                .transpose()
                .map_err(HostError::Settings)?;
            let action_schemas = load_action_schemas(&manifest, |path| {
                let mut matches = entry.action_schemas.iter().filter(|(p, _)| *p == path);
                let (_, text) = matches.next().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "missing action schema asset")
                })?;
                if matches.next().is_some() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "duplicate action schema asset",
                    )
                    .into());
                }
                if text.len() > crate::action_schema::MAX_SCHEMA_BYTES {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "action schema byte budget exceeded",
                    )
                    .into());
                }
                Ok((*text).to_owned())
            })?;
            let compiled_action_schemas = compile_action_schemas(&action_schemas)?;
            let vtable = (entry.vtable)();
            check_descriptor(&vtable)?;
            Ok(LoadedPlugin {
                manifest,
                vtable,
                settings_schema,
                action_schemas,
                compiled_action_schemas,
                namespace_schemas: entry
                    .namespace_schemas
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
                library: None,
                schema_provenance:
                    vosu_plugin_api::manifest::schema::SchemaProvenance::TrustedOfficial,
            })
        })
        .collect()
}

/// Loads one development plugin from `--plugin <dir>` (§7.1, the `--tool <dir>`
/// analog, `src/tools/mod.rs::development_roots`): trusted
/// in-process only in a debug build (`debug_build` is `cfg!(debug_assertions)`
/// at the call site), refused outright in release with an explicit
/// message rather than a silent downgrade.
pub fn load_development(
    application_version: &semver::Version,
    snapshot: &super::host::ValidationSnapshot,
    dir: &Path,
    debug_build: bool,
) -> Result<LoadedPlugin, HostError> {
    if !debug_build {
        return Err(HostError::InProcessRefused {
            reason: "release builds trust only bundled and officially sourced plugins in-process; `--plugin <dir>` is refused",
        });
    }
    let text = std::fs::read_to_string(dir.join("manifest.toml"))?;
    let manifest = parse_plugin(&text, application_version, snapshot, "development", false)?;
    if manifest.runtime != Runtime::InProcess {
        return Err(HostError::InProcessRefused {
            reason: "`--plugin <dir>` only loads an in-process development plugin",
        });
    }
    // A debug override is never installed-source authority.
    load_external(
        manifest,
        dir,
        vosu_plugin_api::manifest::schema::SchemaProvenance::Untrusted,
    )
}

/// Loads one catalog-installed plugin from its committed package directory
/// (catalog I0 runtime handoff). Trust is not decided here: the caller must
/// have run `catalog::trust::CurrentPolicy::authorize_load` for this exact
/// installed artifact under the catalog operation gate immediately before.
/// `official` is that decision's tier (`TrustTier::Official`, i.e. the exact
/// installed source identity is a fixed official one): it selects the SDK
/// context and `TrustedOfficial` schema registration, matching how I0
/// compiled the staged schemas. A trusted user source stays `Untrusted`.
/// A `runtime = "process"` package is refused with
/// `ProcessRuntimeUnavailable`: M5 owns real child execution and the daemon
/// never `dlopen`s it as an in-process substitute.
pub fn load_installed(
    application_version: &semver::Version,
    snapshot: &super::host::ValidationSnapshot,
    dir: &Path,
    source: &str,
    official: bool,
) -> Result<LoadedPlugin, HostError> {
    use std::io::Read as _;
    const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
    let mut text = String::new();
    std::fs::File::open(dir.join("manifest.toml"))?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(HostError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "manifest byte budget exceeded",
        )));
    }
    let manifest = parse_plugin(&text, application_version, snapshot, source, official)?;
    if manifest.runtime != Runtime::InProcess {
        return Err(HostError::ProcessRuntimeUnavailable);
    }
    use vosu_plugin_api::manifest::schema::SchemaProvenance;
    let provenance = if official {
        SchemaProvenance::TrustedOfficial
    } else {
        SchemaProvenance::Untrusted
    };
    load_external(manifest, dir, provenance)
}

fn load_external(
    manifest: PluginManifest,
    dir: &Path,
    schema_provenance: vosu_plugin_api::manifest::schema::SchemaProvenance,
) -> Result<LoadedPlugin, HostError> {
    let root = dir.canonicalize()?;
    let settings_schema = manifest
        .settings_schema
        .as_ref()
        .map(|relative| {
            let text = std::fs::read_to_string(confined_asset(&root, relative)?)?;
            SettingsSchema::parse(&text).map_err(HostError::Settings)
        })
        .transpose()?;
    // Canonical containment also rejects symlinks escaping the plugin root.
    // Manifest validation has already rejected non-relative schema paths.
    let namespace_schemas = manifest
        .provides
        .iter()
        .filter_map(|provided| provided.schema.as_ref())
        .map(|relative| {
            use std::io::Read as _;
            let path = confined_asset(&root, relative)?;
            let limit = vosu_plugin_api::manifest::schema::MAX_BYTES;
            let mut text = String::new();
            std::fs::File::open(path)?
                .take((limit + 1) as u64)
                .read_to_string(&mut text)?;
            if text.len() > limit {
                return Err(HostError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "namespace schema byte budget exceeded",
                )));
            }
            Ok(text)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let action_schemas = load_action_schemas(&manifest, |relative| {
        let path = confined_asset(&root, relative)?;
        use std::io::Read as _;
        let limit = crate::action_schema::MAX_SCHEMA_BYTES;
        let mut text = String::new();
        std::fs::File::open(path)?
            .take((limit + 1) as u64)
            .read_to_string(&mut text)?;
        if text.len() > limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "action schema byte budget exceeded",
            )
            .into());
        }
        Ok(text)
    })?;
    let compiled_action_schemas = compile_action_schemas(&action_schemas)?;
    let triple = current_target_triple();
    let target = manifest
        .targets
        .get(triple)
        .ok_or(HostError::MissingTarget)?;
    let library_path = confined_asset(&root, &target.library)?;
    // SAFETY: `library_path` is confined to a development directory the
    // developer passed via the debug-only `--plugin <dir>` flag, or to a
    // committed catalog package whose exact artifact current §7.1 policy
    // just authorized for in-process execution; running its code in-process
    // is exactly the trust either tier accepts.
    let library = unsafe { Library::new(&library_path) }?;
    // SAFETY: a null-terminated symbol name, looked up on a freshly opened
    // library before any plugin code has run.
    let entry: Symbol<VosuPluginEntryFn> = unsafe { library.get(b"vosu_plugin_entry_v1\0") }?;
    // SAFETY: `vosu_plugin_entry_v1` takes no arguments and is documented
    // to return a pointer valid for the library's whole lifetime, which
    // `library` (kept in the returned `LoadedPlugin`) outlives this call.
    let vtable_ptr = unsafe { entry() };
    if vtable_ptr.is_null() {
        return Err(HostError::MissingEntry);
    }
    // SAFETY: a non-null entry point is documented to point at a live,
    // fully initialized `vosu_plugin_vtable` for the library's lifetime —
    // but only up to the producer's own `struct_size`, which this host
    // must not assume matches `size_of::<vosu_plugin_vtable>()` (§1: a
    // reader never touches a field beyond `struct_size`). `struct_size`
    // and `abi_version` are always the layout's first two `u32` fields, so
    // reading them individually through `addr_of!` is sound even for an
    // undersized descriptor; only once `check_header` confirms
    // `struct_size` covers this host's whole `vosu_plugin_vtable` is the
    // full-struct copy below safe to perform.
    let struct_size = unsafe { std::ptr::addr_of!((*vtable_ptr).struct_size).read() };
    // SAFETY: same reasoning as `struct_size` above.
    let abi_version = unsafe { std::ptr::addr_of!((*vtable_ptr).abi_version).read() };
    check_header(struct_size, abi_version)?;
    // SAFETY: `check_header` just confirmed `struct_size >=
    // size_of::<vosu_plugin_vtable>()`, so this copy never reads past the
    // producer's real descriptor.
    let vtable = unsafe { *vtable_ptr };
    check_descriptor(&vtable)?;
    Ok(LoadedPlugin {
        manifest,
        vtable,
        settings_schema,
        namespace_schemas,
        action_schemas,
        compiled_action_schemas,
        library: Some(Arc::new(library)),
        schema_provenance,
    })
}

/// Parses every `--plugin <dir>`/`--plugin=<dir>` argument, in order
/// (`src/tools/mod.rs::development_roots`'s `--tool` analog).
pub fn development_roots(args: &[OsString]) -> std::io::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--plugin" {
            result.push(PathBuf::from(iter.next().ok_or_else(|| {
                std::io::Error::other("--plugin requires a directory")
            })?));
        } else if let Some(value) = arg.to_str().and_then(|v| v.strip_prefix("--plugin=")) {
            if value.is_empty() {
                return Err(std::io::Error::other("--plugin requires a directory"));
            }
            result.push(value.into());
        }
    }
    result.into_iter().map(std::path::absolute).collect()
}

fn confined_asset(root: &Path, relative: &str) -> Result<PathBuf, HostError> {
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "plugin asset escapes package directory",
        )
        .into());
    }
    Ok(path)
}

fn parse_plugin(
    text: &str,
    application_version: &semver::Version,
    snapshot: &super::host::ValidationSnapshot,
    source: &str,
    official: bool,
) -> Result<PluginManifest, HostError> {
    let package =
        vosu_plugin_api::manifest::PackageManifest::parse(text).map_err(HostError::from)?;
    let support = snapshot
        .validate(&package, application_version, source, official)
        .map_err(HostError::from)?;
    if support != vosu_plugin_api::manifest::HostSupport::Supported {
        return Err(HostError::State(
            "plugin manifest host range does not support this daemon".into(),
        ));
    }
    package.into_plugin().map_err(HostError::from)
}

fn load_action_schemas(
    manifest: &PluginManifest,
    mut read: impl FnMut(&str) -> Result<String, HostError>,
) -> Result<std::collections::BTreeMap<String, String>, HostError> {
    let mut schemas = std::collections::BTreeMap::new();
    for action in &manifest.actions {
        for path in [&action.args_schema, &action.result_schema] {
            if !schemas.contains_key(path) {
                schemas.insert(path.clone(), read(path)?);
            }
        }
    }
    Ok(schemas)
}

fn compile_action_schemas(
    schemas: &std::collections::BTreeMap<String, String>,
) -> Result<
    std::collections::BTreeMap<String, Arc<crate::action_schema::CompiledActionSchema>>,
    HostError,
> {
    schemas
        .iter()
        .map(|(path, text)| {
            crate::action_schema::CompiledActionSchema::compile(text)
                .map(|schema| (path.clone(), Arc::new(schema)))
                .map_err(|error| {
                    HostError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid action schema {path}: {error}"),
                    ))
                })
        })
        .collect()
}

#[cfg(test)]
mod action_tests;
