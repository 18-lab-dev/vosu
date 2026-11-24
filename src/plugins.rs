//! The explicit `BUNDLED` plugin array (D13, `docs/v2/plugin-abi.md` §2):
//! every bundled plugin's vtable-builder function paired with its embedded
//! manifest text (never carrying `[targets.*]`: bundled plugins are linked
//! into this executable, never `dlopen`ed). A linker-collected slice still
//! needs every crate referenced to be linked, so it gives no omission
//! guarantee a compile-time list does not give more simply — forgetting an
//! entry here is a visible, reviewed omission, not a silent one.
//!
//! Production ships `vosu.osu`; the echo fixture is test-harness-only.

#[cfg(feature = "test-harness")]
const TESTKIT_ECHO_MANIFEST: &str = "kind='plugin'\nmanifest_version=1\nid='vosu.testkit.echo'\nname='Testkit Echo'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n";

/// The required `vosu.osu` entry is first; later entries are optional fixtures.
pub static BUNDLED: &[vosu_core::plugin::BundledPlugin<'static>] = &[
    vosu_core::plugin::BundledPlugin {
        manifest: vosu_osu::MANIFEST,
        settings_schema: Some(vosu_osu::SETTINGS_SCHEMA),
        namespace_schemas: &[vosu_osu::OSU_NAMESPACE_SCHEMA],
        action_schemas: &[],
        vtable: vosu_osu::vosu_plugin_vtable,
    },
    #[cfg(feature = "test-harness")]
    vosu_core::plugin::BundledPlugin {
        manifest: TESTKIT_ECHO_MANIFEST,
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: vosu_plugin_testkit::echo::vosu_plugin_vtable,
    },
];
