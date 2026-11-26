use crate::plugin::{BundledPlugin, PluginInstanceId, load_bundled};

#[tokio::test]
async fn vosux_broker_tracks_live_actions_and_stops_on_listener_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let logger = Logger::new(root.path(), LoggerOptions::default(), Arc::new(StdoutConsole)).unwrap();
    let settings = PluginSettingsService::new(Arc::new(crate::plugin::settings::PluginSettingsStore::open(root.path())));
    let host = PluginHost::new(logger.clone(), Arc::new(settings), Handle::current());
    let path = root.path().join("private/broker.json");
    let broker = Arc::new(crate::server::broker::BrokerToken::new(path.clone()).unwrap());
    broker.publish("127.0.0.1:24050".parse().unwrap()).unwrap();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let (desired, _) = tokio::sync::watch::channel(crate::server::runtime::ListenTarget::new(
        "127.0.0.1",
        24050,
    ));
    let (_, state) =
        tokio::sync::watch::channel(crate::server::runtime::ListenerState::default());
    let mut listener = ListenerOwner {
        desired,
        state,
        cancel,
        task: tokio::spawn(async move {
            bounded!(task_cancel.cancelled());
        }),
        broker: Some(broker.clone()),
        broker_logger: None,
        cli_task: None,
    };
    bounded!(listener.watch_cli_actions(&host));
    let read = || -> serde_json::Value {
        serde_json::from_slice(&vosu_sys::platform::read_published(&path).unwrap()).unwrap()
    };
    assert_eq!(read()["actions"], serde_json::json!({}));
    let mut published = broker.publications();
    let manifest = r#"
kind = "plugin"
manifest_version = 1
id = "fixture.c0.metadata"
name = "CLI metadata"
version = "1.0.0"
host = ">=2.0.0-alpha.1"
abi_version = 1
runtime = "in-process"
[[actions]]
name = "nested.echo"
version = "1.0.0"
title = "Echo"
args_schema = "args.json"
result_schema = "result.json"
session = "optional"
cli = true
[[provides]]
id = "action:fixture.c0.metadata.nested.echo"
version = "1.0.0"
[[actions]]
name = "hidden"
version = "1.0.0"
title = "Hidden"
args_schema = "args.json"
result_schema = "result.json"
session = "none"
cli = false
[[provides]]
id = "action:fixture.c0.metadata.hidden"
version = "1.0.0"
"#;
    let loaded = load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[BundledPlugin {
            manifest,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[("args.json", "true"), ("result.json", "true")],
            vtable: vosu_plugin_testkit::echo::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let instance = PluginInstanceId::new(770091);
    host.create_instance(instance, &loaded[0], root.path().join("plugin"), None)
        .unwrap();
    bounded!(published.changed()).unwrap();
    assert_eq!(
        read()["actions"],
        serde_json::json!({"fixture.c0.metadata.nested.echo":"optional"})
    );
    host.action_instance_lost(instance);
    bounded!(published.changed()).unwrap();
    assert_eq!(read()["actions"], serde_json::json!({}));
    let watcher = listener.cli_task.as_ref().unwrap().abort_handle();
    bounded!(listener.shutdown());
    assert!(watcher.is_finished());
    let revision = *published.borrow_and_update();
    host.action_instance_lost(instance);
    assert_eq!(
        *published.borrow(),
        revision,
        "joined watcher cannot republish after shutdown"
    );
    assert!(!published.has_changed().unwrap());
    host.destroy_all(Duration::from_secs(1));
    bounded!(logger.shutdown()).unwrap();
}
