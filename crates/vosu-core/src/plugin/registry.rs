//! Plugin registry: loads the root crate's explicit `BUNDLED` array (D13)
//! and `--plugin <dir>` external `dlopen` plugins (`libloading`, D13), with
//! ABI version negotiation and descriptor checks (`docs/v2/plugin-abi.md`
//! §1, §2) and the §7.1 in-process trust decision.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};
use vosu_plugin_api::{
    PluginManifest, Runtime, VOSU_PLUGIN_ABI_MAJOR, VosuPluginEntryFn, vosu_plugin_vtable,
};

use super::error::HostError;

/// One bundled plugin entry the root crate's explicit `BUNDLED` array lists
/// (D13): its embedded manifest text (never carrying `[targets.*]`, §8) and
/// the plain vtable-builder function `export_plugin!(bundled T)` generates.
#[derive(Clone, Copy)]
pub struct BundledPlugin<'a> {
    pub manifest: &'a str,
    pub vtable: fn() -> vosu_plugin_vtable,
}

/// A manifest-validated, ABI-checked plugin ready for
/// `super::instance::PluginInstance::create`.
pub struct LoadedPlugin {
    manifest: PluginManifest,
    vtable: vosu_plugin_vtable,
    // Kept alive for the plugin's whole lifetime: unloading while an
    // instance's `plugin_ctx` still points into the library would be
    // immediate undefined behavior the next `dispatch`/`destroy` call
    // makes. `None` for a bundled plugin, which is linked into this
    // executable rather than `dlopen`ed.
    #[expect(
        dead_code,
        reason = "held only to keep an external plugin's dlopen'd Library alive for this LoadedPlugin's lifetime; never read directly"
    )]
    library: Option<Library>,
}

impl LoadedPlugin {
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn vtable(&self) -> vosu_plugin_vtable {
        self.vtable
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

fn check_descriptor(vtable: &vosu_plugin_vtable) -> Result<(), HostError> {
    if vtable.abi_version != VOSU_PLUGIN_ABI_MAJOR {
        return Err(HostError::AbiMismatch {
            expected: VOSU_PLUGIN_ABI_MAJOR,
            found: vtable.abi_version,
        });
    }
    let minimum = descriptor_minimum();
    if (vtable.struct_size as usize) < minimum {
        return Err(HostError::DescriptorTooSmall {
            minimum,
            found: vtable.struct_size as usize,
        });
    }
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
            let vtable = (entry.vtable)();
            check_descriptor(&vtable)?;
            Ok(LoadedPlugin {
                manifest,
                vtable,
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
    // fully initialized `vosu_plugin_vtable` for the library's lifetime.
    let vtable = unsafe { *vtable_ptr };
    check_descriptor(&vtable)?;
    Ok(LoadedPlugin {
        manifest,
        vtable,
        library: Some(library),
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
