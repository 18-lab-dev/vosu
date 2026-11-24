//! P0 review findings 1 and 4: `create_instance` must build `init.settings`
//! from the *loaded plugin's own* `settings.schema.json` (not the deleted
//! bridge-B2-only `schema_for`), and `PluginHost`'s
//! [`SettingsSchemaSource`] impl must resolve a plugin id's schema from
//! whichever slot it is currently registered under — `Creating` as well
//! as `Live` — never only the latter (the regression a blocked `create`
//! would otherwise hit: a settings change delivered while still
//! `Creating` resolves no schema, so `effective_settings` built the empty
//! map `§6.9` forbids, queued for replay the moment the instance went
//! `Live`).

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use vosu_plugin_api::{CborValue, Message};

use crate::config::ConfigStore;
use crate::logging::{Logger, LoggerOptions};
use crate::plugin::focus;
use crate::plugin::host::*;
use crate::plugin::registry;
use crate::plugin::settings::{
    BRIDGE_B2_PLUGIN_ID, ConfigSettingsBridgeB2, PluginSettingsService, SettingsSchemaSource,
};

use super::helpers::{SilentConsole, test_host};

/// A minimal game-plugin manifest with a `[match]` table matching
/// nothing (these tests never drive a `ProcessRecord` through it) and a
/// `settings_schema` key, so its `BundledPlugin::settings_schema` entry
/// is consistent (P0 review finding 2: `load_bundled` now refuses a
/// manifest/entry mismatch).
fn schema_manifest(plugin_id: &str) -> String {
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\nsettings_schema='settings.schema.json'\n"
    )
}

/// A manifest with no `settings_schema` key, for a `BundledPlugin`
/// entry whose `settings_schema` is `None` (a consistent, schema-free
/// shape, unlike [`schema_manifest`]).
fn plain_manifest(plugin_id: &str) -> String {
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n"
    )
}

const SCHEMA_TEXT: &str = r#"[{"uniqueID":"RATE","type":"number","value":42.0}]"#;

/// Keyed by plugin id, so each test (and each instance within a test)
/// gets its own slot without the plugins stepping on each other.
fn captured_init_settings() -> &'static Mutex<HashMap<String, CborValue>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, CborValue>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

/// Captures `init.settings` into [`captured_init_settings`] on `create`,
/// so a test can assert on exactly what `create_instance` built — not
/// merely that `create` succeeded.
mod capturing_plugin {
    use vosu_plugin_api::{Init, Plugin, PluginError, VtableHostServices};

    use super::captured_init_settings;

    pub struct CapturingPlugin;

    impl Plugin for CapturingPlugin {
        fn create(_host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
            captured_init_settings()
                .lock()
                .unwrap()
                .insert(init.plugin_id, init.settings);
            Ok(Self)
        }

        fn on_message(
            &mut self,
            _message: vosu_plugin_api::Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }

    vosu_plugin_api::export_plugin!(bundled CapturingPlugin);
}

#[test]
fn create_instance_builds_init_settings_from_the_loaded_plugins_own_schema() {
    let plugin_id = "acme.schema-capture";
    let manifest = schema_manifest(plugin_id);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest,
        settings_schema: Some(SCHEMA_TEXT),
        namespace_schemas: &[],
        vtable: capturing_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the schema-capturing test plugin");

    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let data_dir = tempfile::tempdir().unwrap();
    host.create_instance(
        PluginInstanceId::new(1),
        &loaded[0],
        data_dir.path().to_path_buf(),
        None,
    )
    .expect("creating the schema-capturing instance");

    let captured = captured_init_settings()
        .lock()
        .unwrap()
        .remove(plugin_id)
        .expect("create_instance should have run the plugin's own create with init.settings set");
    let CborValue::Map(entries) = captured else {
        panic!("expected a CBOR map");
    };
    assert!(
        entries
            .iter()
            .any(|(key, value)| *key == CborValue::Text("RATE".into())
                && *value == CborValue::Float(42.0)),
        "expected the schema's own default RATE=42.0 in init.settings, got: {entries:?}"
    );
}

#[test]
fn schema_for_matches_only_the_named_plugin_id_and_none_for_an_unregistered_one() {
    let plugin_id_a = "acme.schema-a";
    let plugin_id_b = "acme.schema-b";
    let manifest_a = schema_manifest(plugin_id_a);
    let manifest_b = plain_manifest(plugin_id_b);
    let loaded_a = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest_a,
        settings_schema: Some(SCHEMA_TEXT),
        namespace_schemas: &[],
        vtable: capturing_plugin::vosu_plugin_vtable,
    }])
    .expect("loading plugin a");
    let loaded_b = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest_b,
        settings_schema: None,
        namespace_schemas: &[],
        vtable: capturing_plugin::vosu_plugin_vtable,
    }])
    .expect("loading plugin b");

    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let data_dir_a = tempfile::tempdir().unwrap();
    let data_dir_b = tempfile::tempdir().unwrap();
    host.create_instance(
        PluginInstanceId::new(1),
        &loaded_a[0],
        data_dir_a.path().to_path_buf(),
        None,
    )
    .expect("creating plugin a's instance");
    host.create_instance(
        PluginInstanceId::new(2),
        &loaded_b[0],
        data_dir_b.path().to_path_buf(),
        None,
    )
    .expect("creating plugin b's instance");

    assert!(
        host.schema_for(plugin_id_a).is_some(),
        "plugin a declared a schema and is Live"
    );
    assert!(
        host.schema_for(plugin_id_b).is_none(),
        "plugin b declared no schema"
    );
    assert!(
        host.schema_for("acme.never-registered").is_none(),
        "no instance of this plugin id was ever created"
    );
}

/// Blocks the gated plugin's own `create` until released, so a test can
/// observe [`PluginHost::schema_for`] resolving the schema while the
/// instance is still registered only under [`InstanceSlot::Creating`]
/// (the review finding: before the fix, this returned `None`).
struct CreateGate {
    release: Mutex<bool>,
    release_cv: Condvar,
}

/// How long [`CreateGate::wait_for_release`] waits before giving up.
/// Bounded deliberately (post-hang-review finding): a test bug driving
/// this gate — an assertion that fires and unwinds before `release()`
/// runs, or a missed notification — must fail with a clear message
/// within this bound, never park the gated plugin's `create` (and
/// `std::thread::scope`'s forced join of the thread running it) forever.
const CREATE_GATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl CreateGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            release: Mutex::new(false),
            release_cv: Condvar::new(),
        })
    }

    fn wait_for_release(&self) {
        let (guard, result) = self
            .release_cv
            .wait_timeout_while(
                self.release.lock().unwrap_or_else(|p| p.into_inner()),
                CREATE_GATE_TIMEOUT,
                |release| !*release,
            )
            .unwrap_or_else(|p| p.into_inner());
        drop(guard);
        assert!(
            !result.timed_out(),
            "CreateGate::wait_for_release timed out after {CREATE_GATE_TIMEOUT:?}: the test \
             driving it never called release() (an assertion likely fired first) — this must \
             never hang"
        );
    }

    fn release(&self) {
        *self.release.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.release_cv.notify_all();
    }
}

fn create_gate_registry() -> &'static Mutex<HashMap<String, Arc<CreateGate>>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, Arc<CreateGate>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

mod gated_plugin {
    use vosu_plugin_api::{Init, Plugin, PluginError, VtableHostServices};

    use super::create_gate_registry;

    pub struct GatedPlugin;

    impl Plugin for GatedPlugin {
        fn create(_host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
            let gate = create_gate_registry()
                .lock()
                .unwrap()
                .get(&init.plugin_id)
                .cloned()
                .unwrap_or_else(|| panic!("no create gate registered for {:?}", init.plugin_id));
            gate.wait_for_release();
            Ok(Self)
        }

        fn on_message(
            &mut self,
            _message: vosu_plugin_api::Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }

    vosu_plugin_api::export_plugin!(bundled GatedPlugin);
}

#[test]
fn schema_for_resolves_a_schema_while_the_instance_is_still_creating() {
    let plugin_id = "acme.schema-gated";
    let gate = CreateGate::new();
    create_gate_registry()
        .lock()
        .unwrap()
        .insert(plugin_id.to_owned(), Arc::clone(&gate));

    let manifest = schema_manifest(plugin_id);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest,
        settings_schema: Some(SCHEMA_TEXT),
        namespace_schemas: &[],
        vtable: gated_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the gated schema test plugin");

    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let data_dir = tempfile::tempdir().unwrap();
    let id = PluginInstanceId::new(1);

    std::thread::scope(|scope| {
        let host_ref = &host;
        let loaded_ref = &loaded[0];
        let data_dir_path = data_dir.path().to_path_buf();
        let handle =
            scope.spawn(move || host_ref.create_instance(id, loaded_ref, data_dir_path, None));

        // Poll until the instance is registered (necessarily `Creating`:
        // `create` is blocked on the gate), then assert `schema_for`
        // already resolves it — the review finding's exact regression,
        // where this returned `None` until `Live`.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(schema) = host.schema_for(plugin_id) {
                assert_eq!(
                    schema.defaults().get("RATE"),
                    Some(&serde_json::Value::from(42.0)),
                    "resolved the wrong schema"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "schema_for never resolved the still-Creating instance's schema"
            );
            std::thread::yield_now();
        }

        gate.release();
        vosu_proptest::wait::JoinBounded::join_bounded(handle)
            .unwrap()
            .expect("creating the gated schema instance");
    });
}

/// Every `settings.schema.json` entry bridge B2's five `vosu.toml` keys
/// had (mirroring `config/schema.rs`'s own defaults, the same shape
/// `ConfigSettingsBridgeB2::load` always returns regardless of what
/// schema asked for it): needed here because the test below drives a
/// real [`ConfigSettingsBridgeB2`], whose `load` is not parameterized by
/// schema.
const B2_SHAPED_SCHEMA_TEXT: &str = r#"[
    {"uniqueID":"CALCULATE_PP","type":"checkbox","title":"Calculate pp","value":true},
    {"uniqueID":"POLL_RATE","type":"number","title":"Poll rate","value":150.0},
    {"uniqueID":"PRECISE_DATA_POLL_RATE","type":"number","title":"Precise data poll rate","value":10.0},
    {"uniqueID":"SHOW_MP_COMMANDS","type":"checkbox","title":"Show multiplayer commands","value":false},
    {"uniqueID":"READ_MANIA_SCROLL_SPEED","type":"checkbox","title":"Read mania scroll speed","value":true}
]"#;

/// Keyed by plugin id, so a test can assert on exactly which
/// `SETTINGS_CHANGED` messages a plugin received (including a replayed
/// one once it goes `Live`).
fn captured_settings_changed() -> &'static Mutex<HashMap<String, std::sync::mpsc::Sender<Message>>>
{
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, std::sync::mpsc::Sender<Message>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

/// Blocks `create` on [`CreateGate`] (same registry as the plugins
/// above), and forwards every message it is dispatched (in particular a
/// replayed `SETTINGS_CHANGED`) to [`captured_settings_changed`].
mod gated_replay_plugin {
    use vosu_plugin_api::{Init, Message, Plugin, PluginError, VtableHostServices};

    use super::{captured_settings_changed, create_gate_registry};

    pub struct GatedReplayPlugin {
        plugin_id: String,
    }

    impl Plugin for GatedReplayPlugin {
        fn create(_host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
            let gate = create_gate_registry()
                .lock()
                .unwrap()
                .get(&init.plugin_id)
                .cloned()
                .unwrap_or_else(|| panic!("no create gate registered for {:?}", init.plugin_id));
            gate.wait_for_release();
            Ok(Self {
                plugin_id: init.plugin_id,
            })
        }

        fn on_message(
            &mut self,
            message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            if let Some(tx) = captured_settings_changed()
                .lock()
                .unwrap()
                .get(&self.plugin_id)
            {
                let _ = tx.send(message);
            }
            Ok(())
        }
    }

    vosu_plugin_api::export_plugin!(bundled GatedReplayPlugin);
}

/// P0 review finding 1, regression test: before the fix, `schema_for`
/// resolved only `Live` instances, so a B2 key change arriving while
/// `vosu.osu` was still `Creating` resolved no schema, built the empty
/// `CborValue::Map([])` `effective_settings` falls back to, and queued
/// *that* for replay — the plugin would have gone `Live` and immediately
/// received an empty, spec-violating effective settings object instead
/// of the real one. This drives a real [`ConfigSettingsBridgeB2`] (not a
/// `FixedSchemaSource` test double) through [`PluginHost::watch_bridge_b2`]
/// while the instance is provably still `Creating`, and asserts the
/// replayed message — the one the plugin actually receives once `Live`
/// — carries the complete, up-to-date five-key map.
#[test]
fn a_b2_change_during_a_blocked_create_delivers_the_full_map_once_live() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();

    let config_dir = tempfile::tempdir().unwrap();
    let config = runtime.block_on(async {
        std::sync::Arc::new(
            ConfigStore::initialize(&config_dir.path().join("vosu.toml"))
                .await
                .unwrap(),
        )
    });
    let bridge = Arc::new(ConfigSettingsBridgeB2::new(Arc::clone(&config)));
    let settings = Arc::new(PluginSettingsService::new(bridge.clone()));

    let log_dir = tempfile::tempdir().unwrap();
    let logger = Logger::new(
        log_dir.path(),
        LoggerOptions {
            version: "0.1.0".to_owned(),
            debug_log: false,
            queue_capacity: 64,
        },
        Arc::new(SilentConsole),
    )
    .unwrap();
    let host = PluginHost::new_for_tests(
        logger,
        settings,
        runtime.handle().clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
        crate::plugin::session::IDENTIFICATION_TIMEOUT,
    );
    let mut shutdown =
        crate::plugin::test_support::ConfigHostShutdown::new(&runtime, &config, &host);

    let gate = CreateGate::new();
    create_gate_registry()
        .lock()
        .unwrap()
        .insert(BRIDGE_B2_PLUGIN_ID.to_owned(), Arc::clone(&gate));
    let (tx, rx) = std::sync::mpsc::channel();
    captured_settings_changed()
        .lock()
        .unwrap()
        .insert(BRIDGE_B2_PLUGIN_ID.to_owned(), tx);

    let manifest = schema_manifest(BRIDGE_B2_PLUGIN_ID);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest,
        settings_schema: Some(B2_SHAPED_SCHEMA_TEXT),
        namespace_schemas: &[],
        vtable: gated_replay_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the gated replay test plugin");

    let data_dir = tempfile::tempdir().unwrap();
    let id = PluginInstanceId::new(1);

    std::thread::scope(|scope| {
        let host_ref = &host;
        let loaded_ref = &loaded[0];
        let data_dir_path = data_dir.path().to_path_buf();
        let create_thread =
            scope.spawn(move || host_ref.create_instance(id, loaded_ref, data_dir_path, None));

        // Bug found by a 1-in-6 hang (Main's report): without this
        // wait, the config change below can race ahead of
        // `create_instance` even registering `id`'s `Creating`
        // placeholder at all (it is registered inline, but on another
        // thread, with no happens-before edge to this one). `dispatch_to`
        // never retries a message that found no matching instance, so
        // the delivery below is then silently and permanently lost, the
        // "queued" wait times out, its `assert!` panics, and — because
        // that panic unwinds out of this `scope` closure while
        // `create_thread` is still parked in `CreateGate::wait_for_release`
        // — `std::thread::Scope`'s forced join of that thread (required
        // before the panic may propagate) hung forever pre-fix. Waiting
        // here for the placeholder to exist first removes the race
        // outright; `CreateGate::wait_for_release`'s own bound (above)
        // is the fallback that turns any future instance of this into a
        // fast, readable failure instead of a second hang.
        let registered_deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let registered = matches!(
                host.instances
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&id),
                Some(InstanceSlot::Creating { .. })
            );
            if registered {
                break;
            }
            assert!(
                std::time::Instant::now() < registered_deadline,
                "expected create_instance to register id's Creating placeholder within 5s"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        shutdown.set_watcher(host.watch_bridge_b2(&bridge, &runtime.handle().clone()));

        // A plain `ConfigStore::update`, exactly the shape a dashboard
        // write or a `vosu.toml` hot reload uses.
        config.update(|c| {
            c.poll_rate = 275.0;
        });

        // Wait until the watcher's delivery has actually been queued
        // against the still-`Creating` placeholder — proof the change
        // reached it before `create` returns, not scheduling luck.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let queued = {
                let guard = host.instances.lock().unwrap_or_else(|p| p.into_inner());
                matches!(
                    guard.get(&id),
                    Some(InstanceSlot::Creating { pending, .. }) if !pending.is_empty()
                )
            };
            if queued {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected the B2 change to queue against the Creating placeholder within 5s"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        gate.release();
        vosu_proptest::wait::JoinBounded::join_bounded(create_thread)
            .unwrap()
            .expect("creating the gated replay instance");
    });

    // Settle persistence before teardown. Without explicit shutdown, this
    // leaves the blocking store worker idle on commands (no debounce timer
    // to panic during runtime shutdown), deterministically exposing the hang.
    runtime
        .block_on(config.flush())
        .expect("flushing B2 config");

    let message = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("expected the replayed SETTINGS_CHANGED once the instance went Live");
    let Message::SettingsChanged(changed) = message else {
        panic!("expected SettingsChanged");
    };
    let CborValue::Map(entries) = changed.values else {
        panic!("expected a CBOR map");
    };
    assert!(
        !entries.is_empty(),
        "the replayed delivery must never be the empty map (§6.9)"
    );
    assert!(
        entries.iter().any(|(key, value)| {
            *key == CborValue::Text("POLL_RATE".into()) && *value == CborValue::Float(275.0)
        }),
        "expected the real, current POLL_RATE=275.0, got: {entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|(key, _)| *key == CborValue::Text("CALCULATE_PP".into())),
        "expected the complete five-key effective settings object, got: {entries:?}"
    );
}
