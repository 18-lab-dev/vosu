//! The explicit `BUNDLED` plugin array (D13, `docs/v2/plugin-abi.md` §2):
//! every bundled plugin's vtable-builder function paired with its embedded
//! manifest text (never carrying `[targets.*]`: bundled plugins are linked
//! into this executable, never `dlopen`ed). A linker-collected slice still
//! needs every crate referenced to be linked, so it gives no omission
//! guarantee a compile-time list does not give more simply — forgetting an
//! entry here is a visible, reviewed omission, not a silent one.
//!
//! Production ships the game provider followed by three required projections
//! and the dashboard host (registered even when `ENABLE_DASHBOARD` is off).

#[cfg(feature = "test-harness")]
const TESTKIT_ECHO_MANIFEST: &str = "kind='plugin'\nmanifest_version=1\nid='vosu.testkit.echo'\nname='Testkit Echo'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n";

/// Required game/provider entries precede optional test fixtures.
pub static BUNDLED: &[vosu_core::plugin::BundledPlugin<'static>] = &[
    vosu_core::plugin::BundledPlugin {
        manifest: vosu_osu::MANIFEST,
        settings_schema: Some(vosu_osu::SETTINGS_SCHEMA),
        namespace_schemas: &[vosu_osu::OSU_NAMESPACE_SCHEMA],
        action_schemas: vosu_osu::ACTION_SCHEMAS,
        vtable: vosu_osu::vosu_plugin_vtable,
    },
    vosu_core::plugin::BundledPlugin {
        manifest: vosu_adapter_upstream::MANIFEST,
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: vosu_adapter_upstream::vosu_plugin_vtable,
    },
    vosu_core::plugin::BundledPlugin {
        manifest: vosu_adapter_gosumemory::MANIFEST,
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: vosu_adapter_gosumemory::vosu_plugin_vtable,
    },
    vosu_core::plugin::BundledPlugin {
        manifest: vosu_adapter_streamcompanion::MANIFEST,
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: vosu_adapter_streamcompanion::vosu_plugin_vtable,
    },
    vosu_core::plugin::BundledPlugin {
        manifest: vosu_dashboard::MANIFEST,
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: vosu_dashboard::vosu_plugin_vtable,
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
