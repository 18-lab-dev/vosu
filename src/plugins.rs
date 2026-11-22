//! The explicit `BUNDLED` plugin array (D13, `docs/v2/plugin-abi.md` §2):
//! every bundled plugin's vtable-builder function paired with its embedded
//! manifest text (never carrying `[targets.*]`: bundled plugins are linked
//! into this executable, never `dlopen`ed). A linker-collected slice still
//! needs every crate referenced to be linked, so it gives no omission
//! guarantee a compile-time list does not give more simply — forgetting an
//! entry here is a visible, reviewed omission, not a silent one.
//!
//! The only bundled plugin so far is the `test-harness`-only testkit echo
//! plugin; the osu! plugin and the first-party surfaces join this array in
//! later milestones.

#[cfg(feature = "test-harness")]
const TESTKIT_ECHO_MANIFEST: &str = "kind='plugin'\nmanifest_version=1\nid='vosu.testkit.echo'\nname='Testkit Echo'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n";

#[cfg(feature = "test-harness")]
pub static BUNDLED: &[vosu_core::plugin::BundledPlugin<'static>] =
    &[vosu_core::plugin::BundledPlugin {
        manifest: TESTKIT_ECHO_MANIFEST,
        vtable: vosu_plugin_testkit::echo::vosu_plugin_vtable,
    }];

#[cfg(not(feature = "test-harness"))]
pub static BUNDLED: &[vosu_core::plugin::BundledPlugin<'static>] = &[];
