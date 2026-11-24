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
    // Shared, not just held: unloading while an instance's `plugin_ctx`
    // still points into the library would be immediate undefined behavior
    // the next `dispatch`/`destroy` call makes, and `LoadedPlugin` alone
    // does not outlive the instances built from it (a caller is free to
    // drop it right after `PluginInstance::create`). `PluginInstance`
    // clones this `Arc` via `library_handle` so the library stays mapped
    // for as long as either side needs it. `None` for a bundled plugin,
    // which is linked into this executable rather than `dlopen`ed.
    library: Option<Arc<Library>>,
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

    /// Clones this plugin's `Arc<Library>` handle, so a `PluginInstance`
    /// can co-own it for its whole lifetime, independent of how long this
    /// `LoadedPlugin` itself stays alive. `None` for a bundled plugin.
    pub(crate) fn library_handle(&self) -> Option<Arc<Library>> {
        self.library.clone()
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
pub fn load_bundled(bundled: &[BundledPlugin<'_>]) -> Result<Vec<LoadedPlugin>, HostError> {
    bundled
        .iter()
        .map(|entry| {
            let manifest = PluginManifest::parse(entry.manifest)?;
            if manifest.runtime != Runtime::InProcess {
                return Err(HostError::InProcessRefused {
                    reason: "a bundled plugin's manifest must request runtime = \"in-process\"",
                });
            }
            let settings_schema = entry
                .settings_schema
                .map(SettingsSchema::parse)
                .transpose()
                .map_err(HostError::Settings)?;
            let vtable = (entry.vtable)();
            check_descriptor(&vtable)?;
            Ok(LoadedPlugin {
                manifest,
                vtable,
                settings_schema,
                library: None,
            })
        })
        .collect()
}

/// Loads one development plugin from `--plugin <dir>` (§7.1, the `--tool
/// <dir>` analog, `src/tools/mod.rs::development_roots`): trusted
/// in-process only in a debug build (`debug_build` is `cfg!(debug_assertions)`
/// at the call site), refused outright in release with an explicit
/// message rather than a silent downgrade.
pub fn load_development(dir: &Path, debug_build: bool) -> Result<LoadedPlugin, HostError> {
    if !debug_build {
        return Err(HostError::InProcessRefused {
            reason: "release builds trust only bundled and officially sourced plugins in-process; `--plugin <dir>` is refused",
        });
    }
    let text = std::fs::read_to_string(dir.join("manifest.toml"))?;
    let manifest = PluginManifest::parse(&text)?;
    if manifest.runtime != Runtime::InProcess {
        return Err(HostError::InProcessRefused {
            reason: "`--plugin <dir>` only loads an in-process development plugin",
        });
    }
    let settings_schema = manifest
        .settings_schema
        .as_ref()
        .map(|relative| {
            let text = std::fs::read_to_string(dir.join(relative))?;
            SettingsSchema::parse(&text).map_err(HostError::Settings)
        })
        .transpose()?;
    let triple = current_target_triple();
    let target = manifest
        .targets
        .get(triple)
        .ok_or(HostError::MissingTarget)?;
    let library_path = dir.join(&target.library);
    // SAFETY: `library_path` names a file the developer passed via the
    // debug-only `--plugin <dir>` flag; loading and running its code
    // in-process is exactly the trust this development tier accepts
    // (§7.1), the whole reason this path is refused outside debug builds.
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
        library: Some(Arc::new(library)),
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
