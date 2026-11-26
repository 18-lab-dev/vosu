//! The explicit `BUNDLED` plugin array (D13, `docs/v2/plugin-abi.md` §2):
//! every bundled plugin's vtable-builder function paired with its embedded
//! manifest text (never carrying `[targets.*]`: bundled plugins are linked
//! into this executable, never `dlopen`ed). A linker-collected slice still
//! needs every crate referenced to be linked, so it gives no omission
//! guarantee a compile-time list does not give more simply — forgetting an
//! entry here is a visible, reviewed omission, not a silent one.
//!
//! Production ships the game provider, three required projections, the
//! dashboard host (registered even when `ENABLE_DASHBOARD` is off), the
//! overlay host and the web tool host.

use vosu_core::runtime::{Bundle, OverlayRootSetting};

/// The executable's version, startup identity and concrete bundled hosts.
pub static BUNDLE: Bundle = Bundle {
    version: env!("CARGO_PKG_VERSION"),
    // A child started from a replaced executable refuses to mix generations.
    plugin_host_identity: concat!("vosu/", env!("CARGO_PKG_VERSION"), "/plugin-host-1"),
    plugins: BUNDLED,
    plugin_ids: &[
        "vosu.osu",
        "vosu.adapter.upstream",
        "vosu.adapter.gosumemory",
        "vosu.adapter.streamcompanion",
        "vosu.dashboard",
        "vosu.overlayhost",
        "vosu.toolhost",
        #[cfg(feature = "test-harness")]
        "vosu.testkit.echo",
    ],
    dashboard: "vosu.dashboard",
    tool_host: "vosu.toolhost",
    overlay_root: OverlayRootSetting {
        plugin: "vosu.overlayhost",
        key: vosu_overlay_host::root::STATIC_FOLDER_PATH,
    },
    hosts: &crate::bundle::Hosts,
};

#[cfg(feature = "test-harness")]
const TESTKIT_ECHO_MANIFEST: &str = "kind='plugin'\nmanifest_version=1\nid='vosu.testkit.echo'\nname='Testkit Echo'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n";

/// Production entries in instance order, followed by the harness-only echo.
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
    // The overlay host's instance id is its position in BUNDLE.plugin_ids.
    vosu_core::plugin::BundledPlugin {
        manifest: vosu_overlay_host::MANIFEST,
        settings_schema: Some(vosu_overlay_host::SETTINGS_SCHEMA),
        namespace_schemas: &[],
        action_schemas: vosu_overlay_host::ACTION_SCHEMAS,
        vtable: vosu_overlay_host::vosu_plugin_vtable,
    },
    vosu_core::plugin::BundledPlugin {
        manifest: vosu_tool_host::MANIFEST,
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: vosu_tool_host::vosu_plugin_vtable,
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
