//! The one descriptor-validation path (`docs/v2/plugin-abi.md` §1, §2):
//! header/minimum-size/required-function checks for a bundled vtable, and
//! the `dlopen` → `vosu_plugin_entry_v1` → validated-copy sequence shared
//! by the local development grant and the `vosu plugin-host` child (§7.2:
//! "validates `abi_version`/`struct_size` exactly as an in-process load
//! would").

use std::path::Path;
use std::sync::Arc;

use libloading::{Library, Symbol};
use vosu_plugin_api::{VOSU_PLUGIN_ABI_MAJOR, VosuPluginEntryFn, vosu_plugin_vtable};

use super::LocalCode;
use crate::plugin::error::HostError;

/// The minimum `struct_size` this major ABI version accepts: large enough
/// to hold the three required functions `create`, `dispatch`, `destroy`
/// (§1). Only one minor revision exists so far, so this equals the full
/// Rust struct size; a future additive minor revision keeps this constant
/// at the size this host understands, never the newer producer's size.
pub(crate) fn descriptor_minimum() -> usize {
    core::mem::size_of::<vosu_plugin_vtable>()
}

pub(crate) fn check_header(struct_size: u32, abi_version: u32) -> Result<(), HostError> {
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

pub(crate) fn check_descriptor(vtable: &vosu_plugin_vtable) -> Result<(), HostError> {
    check_header(vtable.struct_size, vtable.abi_version)?;
    if vtable.create.is_none() || vtable.dispatch.is_none() || vtable.destroy.is_none() {
        return Err(HostError::MissingRequiredFunction);
    }
    Ok(())
}

/// Maps `path`, resolves `vosu_plugin_entry_v1`, and returns its validated
/// vtable together with the library handle that keeps it mapped.
///
/// # Safety
///
/// Mapping a library runs its static constructors, and the returned vtable
/// calls its code: the caller must already hold the trust to execute this
/// library in the current process. That is the debug-only local
/// development grant (§7.1) in core, or the `vosu plugin-host` child after
/// its capability drop or job confinement (§7.3, §7.4) — never core
/// validating a `runtime = "process"` package.
pub(crate) unsafe fn load_library(path: &Path) -> Result<LocalCode, HostError> {
    // SAFETY: the caller holds the trust to run this library's code in the
    // current process (this function's own contract).
    let library = unsafe { Library::new(path) }?;
    // SAFETY: a null-terminated symbol name, looked up on a freshly opened
    // library before any plugin entry point has run.
    let entry: Symbol<VosuPluginEntryFn> = unsafe { library.get(b"vosu_plugin_entry_v1\0") }?;
    // SAFETY: `vosu_plugin_entry_v1` takes no arguments and is documented
    // to return a pointer valid for the library's whole lifetime, which
    // `library` (kept in the returned `LocalCode`) outlives this call.
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
    Ok(LocalCode {
        vtable,
        library: Some(Arc::new(library)),
    })
}
