//! Plugin registry: validates the root crate's explicit `BUNDLED` array
//! (D13), `--plugin <dir>` development packages and committed catalog
//! packages into one [`LoadedPlugin`] shape — manifest, schemas and grants
//! through one validation path — then applies the §7.1 runtime decision
//! (`docs/v2/plugin-abi.md` §1, §2, §7.1). Only a local grant enters native
//! code here (a linked bundled vtable, or a development or policy-authorized
//! installed library `dlopen`ed through `execution::descriptor::load_library`).
//! A `runtime = "process"` development package yields a confined
//! [`super::execution::ProcessPackage`]; its library is never mapped in
//! core.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::Digest as _;
use vosu_plugin_api::manifest::action_schema::{self, CompiledActionSchema};
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_plugin_api::{PluginManifest, Runtime, vosu_plugin_vtable};

use super::approval::Provenance;
use super::error::HostError;
use super::execution::{Execution, LocalCode, PackageOrigin, ProcessPackage, descriptor};

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

/// A manifest-validated package with its one granted [`Execution`],
/// ready for `super::instance::PluginInstance::create`.
#[derive(Clone)]
pub struct LoadedPlugin {
    manifest: PluginManifest,
    origin: PackageOrigin,
    execution: Execution,
    /// This plugin's own parsed `settings.schema.json` (§6.9), if it
    /// declares one: `create_instance` builds `init.settings` against it
    /// and keeps a copy for bridge-style settings-change watchers (§8's
    /// `settings_schema` manifest field; `BundledPlugin::settings_schema`/
    /// `load_development`'s `manifest.settings_schema` path).
    settings_schema: Option<SettingsSchema>,
    namespace_schemas: Vec<String>,
    action_schemas: std::collections::BTreeMap<String, String>,
    compiled_action_schemas: std::collections::BTreeMap<String, Arc<CompiledActionSchema>>,
}

impl LoadedPlugin {
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// Where this package came from. Schema trust and bundled-only grants
    /// key off this, never off the execution backend.
    pub fn origin(&self) -> PackageOrigin {
        self.origin
    }

    /// Provenance the state registry compiles this plugin's namespace
    /// schemas with (`x-visibility` requires `TrustedOfficial`): only the
    /// compiled `BUNDLED` array, catalog packages installed from an exact
    /// official source identity (the rule catalog I0 compiles staged schemas
    /// with) and the test harness's official fixture
    /// ([`PackageOrigin::is_official`]). Debug `--plugin` overrides and
    /// trusted user-source packages stay `Untrusted`; a dlopened library
    /// never implies official authority.
    pub fn schema_provenance(&self) -> vosu_plugin_api::manifest::schema::SchemaProvenance {
        use vosu_plugin_api::manifest::schema::SchemaProvenance;
        if self.origin.is_official() {
            SchemaProvenance::TrustedOfficial
        } else {
            SchemaProvenance::Untrusted
        }
    }

    /// How this package runs: local code, or a process package core never
    /// enters. A local development library stays mapped for as long as
    /// either this value or an instance built from it is alive.
    pub fn execution(&self) -> &Execution {
        &self.execution
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
    ) -> &std::collections::BTreeMap<String, Arc<CompiledActionSchema>> {
        &self.compiled_action_schemas
    }
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
                .map_err(|e| HostError::Settings(e.into()))?;
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
                if text.len() > action_schema::MAX_SCHEMA_BYTES {
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
            descriptor::check_descriptor(&vtable)?;
            Ok(LoadedPlugin {
                manifest,
                origin: PackageOrigin::Bundled,
                execution: Execution::Local(LocalCode {
                    vtable,
                    library: None,
                }),
                settings_schema,
                action_schemas,
                compiled_action_schemas,
                namespace_schemas: entry
                    .namespace_schemas
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
            })
        })
        .collect()
}

/// Loads one development package from `--plugin <dir>` (§7.1, the
/// `--tool <dir>` analog, `src/tools/mod.rs::development_roots`), under the
/// build policy of catalog-v2.md A5.5:
///
/// - **Debug** (`debug_build` is `cfg!(debug_assertions)` at the call
///   site): a `runtime = "in-process"` request gets the documented
///   developer exception and its library is loaded here.
/// - **Prerelease release** (not debug, and `application_version`, the
///   executable's own version, has a semver prerelease part): every
///   package is `user` trust. An `in-process` request is refused with
///   [`HostError::InProcessRefused`] before any asset is read; it is never
///   rewritten to `process`.
/// - **Stable release**: every package is refused with
///   [`HostError::DevelopmentRefused`].
///
/// A `runtime = "process"` request is validated identically in every
/// allowed build and returned as a process package whose library core
/// never maps. It runs only after approval, which the launcher installed
/// by composition enforces (`plugin::approval`).
pub fn load_development(
    application_version: &semver::Version,
    snapshot: &super::host::ValidationSnapshot,
    dir: &Path,
    debug_build: bool,
) -> Result<LoadedPlugin, HostError> {
    if !debug_build && application_version.pre.is_empty() {
        return Err(HostError::DevelopmentRefused);
    }
    load_package_dir(
        application_version,
        snapshot,
        dir,
        PackageOrigin::Development,
        debug_build,
    )
}

/// The test harness's official loading seam (M5 X0): loads the package in
/// `dir` under its own official `vosu.*` manifest, so a native test runs
/// the separately staged external osu! producer
/// (`crates/vosu-osu/examples/osu_process_plugin.rs`) with the real
/// `vosu.osu` identity in a plugin host. The ordinary development loader
/// refuses that identity (M6a Q3: officialness is explicit provenance).
///
/// Never reachable from a release build, three times over: it exists only
/// with `test-harness`, which `vosu-core` and the root build script reject
/// in release builds; it refuses outright unless `debug_build`
/// (`cfg!(debug_assertions)` at the call site); and only a
/// `runtime = "process"` package is accepted, so it never maps a library
/// in core. The package is [`PackageOrigin::OfficialTestFixture`]:
/// official namespace and schema trust, never the bundled-only grants.
#[cfg(feature = "test-harness")]
pub fn load_official_test_fixture(
    application_version: &semver::Version,
    snapshot: &super::host::ValidationSnapshot,
    dir: &Path,
    debug_build: bool,
) -> Result<LoadedPlugin, HostError> {
    if !debug_build {
        return Err(HostError::InProcessRefused {
            reason: "the test harness's official fixture loading is refused outside debug builds",
        });
    }
    load_package_dir(
        application_version,
        snapshot,
        dir,
        PackageOrigin::OfficialTestFixture,
        debug_build,
    )
}

/// Validates the package in `dir` for `origin` and grants its runtime,
/// under `load_development`'s build policy for `debug_build`. Only a
/// [`PackageOrigin::Development`] package in a debug build reaches the
/// in-process arm: the official test fixture is refused before any library
/// is mapped. Neither origin is approved here: a process package runs only
/// through the installed launcher, which production composition makes the
/// approval gate (`plugin::approval`) for every package alike.
fn load_package_dir(
    application_version: &semver::Version,
    snapshot: &super::host::ValidationSnapshot,
    dir: &Path,
    origin: PackageOrigin,
    debug_build: bool,
) -> Result<LoadedPlugin, HostError> {
    let text = std::fs::read_to_string(dir.join("manifest.toml"))?;
    let manifest = parse_plugin(
        &text,
        application_version,
        snapshot,
        "development",
        origin.is_official(),
    )?;
    #[cfg(feature = "test-harness")]
    if origin == PackageOrigin::OfficialTestFixture && manifest.runtime != Runtime::Process {
        return Err(HostError::InProcessRefused {
            reason: "the test harness's official fixture loading accepts only `runtime = \"process\"` packages",
        });
    }
    if !debug_build && manifest.runtime == Runtime::InProcess {
        return Err(HostError::InProcessRefused {
            reason: "a release build runs `--plugin` packages as user trust, which never grants in-process; request `runtime = \"process\"`",
        });
    }
    load_package(manifest, &text, dir, origin, None)
}

/// Loads one catalog-installed plugin from its committed package directory
/// (catalog I0 runtime handoff). Trust is not decided here: the caller must
/// have run `catalog::trust::CurrentPolicy::authorize_load` for this exact
/// installed artifact under the catalog operation gate immediately before.
/// `official` is that decision's tier (`TrustTier::Official`, i.e. the exact
/// installed source identity is a fixed official one): it selects the SDK
/// context and, through [`PackageOrigin::Installed`], `TrustedOfficial`
/// schema registration, matching how I0 compiled the staged schemas. A
/// trusted user source stays `Untrusted`. This entry point accepts only
/// in-process packages; catalog process packages have a separate loader
/// that cannot map the library in core.
pub fn load_installed(
    application_version: &semver::Version,
    snapshot: &super::host::ValidationSnapshot,
    dir: &Path,
    source: &str,
    official: bool,
) -> Result<LoadedPlugin, HostError> {
    let text = read_bounded_asset(
        &dir.canonicalize()?,
        "manifest.toml",
        vosu_plugin_api::manifest::files::MAX_MANIFEST_BYTES,
        "manifest byte budget exceeded",
    )?;
    let manifest = parse_plugin(&text, application_version, snapshot, source, official)?;
    if manifest.runtime != Runtime::InProcess {
        return Err(HostError::ProcessRuntimeUnavailable);
    }
    load_package(
        manifest,
        &text,
        dir,
        PackageOrigin::Installed { official },
        None,
    )
}

/// Validates a committed process package without opening it as executable
/// code. Its installed identity comes from the record, not the directory
/// name or manifest. Launch authority is checked separately.
pub(crate) fn load_installed_process(
    application_version: &semver::Version,
    snapshot: &super::host::ValidationSnapshot,
    dir: &Path,
    expected_id: &str,
    provenance: Provenance,
    official: bool,
) -> Result<LoadedPlugin, HostError> {
    let Provenance::Installed {
        source, version, ..
    } = &provenance
    else {
        return Err(HostError::State(
            "expected installed plugin provenance".into(),
        ));
    };
    let text = read_bounded_asset(
        &dir.canonicalize()?,
        "manifest.toml",
        vosu_plugin_api::manifest::files::MAX_MANIFEST_BYTES,
        "manifest byte budget exceeded",
    )?;
    let manifest = parse_plugin(&text, application_version, snapshot, source, official)?;
    if manifest.id != expected_id
        || manifest.version != *version
        || manifest.runtime != Runtime::Process
    {
        return Err(HostError::State(
            "installed process plugin differs from its recorded identity or runtime".into(),
        ));
    }
    load_package(
        manifest,
        &text,
        dir,
        PackageOrigin::Installed { official },
        Some(provenance),
    )
}

/// Reads one validated external package's confined assets and attaches its
/// granted execution: a development or policy-authorized installed
/// in-process library is mapped here; a process library is only recorded.
fn load_package(
    manifest: PluginManifest,
    manifest_text: &str,
    dir: &Path,
    origin: PackageOrigin,
    installed: Option<Provenance>,
) -> Result<LoadedPlugin, HostError> {
    let root = dir.canonicalize()?;
    let settings_schema = manifest
        .settings_schema
        .as_ref()
        .map(|relative| {
            let text = std::fs::read_to_string(confined_asset(&root, relative)?)?;
            SettingsSchema::parse(&text).map_err(|e| HostError::Settings(e.into()))
        })
        .transpose()?;
    // Canonical containment also rejects symlinks escaping the plugin root.
    // Manifest validation has already rejected non-relative schema paths.
    let namespace_schemas = manifest
        .provides
        .iter()
        .filter_map(|provided| provided.schema.as_ref())
        .map(|relative| {
            read_bounded_asset(
                &root,
                relative,
                vosu_plugin_api::manifest::schema::MAX_BYTES,
                "namespace schema byte budget exceeded",
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let action_schemas = load_action_schemas(&manifest, |relative| {
        read_bounded_asset(
            &root,
            relative,
            action_schema::MAX_SCHEMA_BYTES,
            "action schema byte budget exceeded",
        )
    })?;
    let compiled_action_schemas = compile_action_schemas(&action_schemas)?;
    let target = manifest
        .targets
        .get(current_target_triple())
        .ok_or(HostError::MissingTarget)?;
    let library_path = confined_asset(&root, &target.library)?;
    let execution = match manifest.runtime {
        // SAFETY: `library_path` is confined to a development directory the
        // developer passed via `--plugin <dir>` in a debug build (release
        // builds refuse a development in-process request in
        // `load_development`), or to a committed catalog package whose exact
        // artifact current §7.1 policy just authorized for in-process
        // execution (`load_installed` refuses every other runtime before this
        // point); loading and running its code in-process is exactly the
        // trust either tier accepts.
        Runtime::InProcess => Execution::Local(unsafe { descriptor::load_library(&library_path) }?),
        Runtime::Process => {
            let manifest_sha256: [u8; 32] = sha2::Sha256::digest(manifest_text.as_bytes()).into();
            Execution::Process(ProcessPackage::new(
                root,
                library_path,
                manifest_sha256,
                installed,
            ))
        }
    };
    Ok(LoadedPlugin {
        manifest,
        origin,
        execution,
        settings_schema,
        namespace_schemas,
        action_schemas,
        compiled_action_schemas,
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

/// Reads one confined text asset, refusing more than `limit` bytes without
/// buffering past it.
fn read_bounded_asset(
    root: &Path,
    relative: &str,
    limit: usize,
    exceeded: &'static str,
) -> Result<String, HostError> {
    use std::io::Read as _;
    let path = confined_asset(root, relative)?;
    let mut text = String::new();
    std::fs::File::open(path)?
        .take((limit + 1) as u64)
        .read_to_string(&mut text)?;
    if text.len() > limit {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, exceeded).into());
    }
    Ok(text)
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
) -> Result<std::collections::BTreeMap<String, Arc<CompiledActionSchema>>, HostError> {
    schemas
        .iter()
        .map(|(path, text)| {
            CompiledActionSchema::compile(text)
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
#[cfg(all(test, feature = "test-harness"))]
mod official_fixture_tests;
