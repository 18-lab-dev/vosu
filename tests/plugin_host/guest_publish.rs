//! Non-owner own-namespace publication (`docs/v2/plugin-abi.md` §6.6/§6.7,
//! 2026-11-26) end to end through the public host API: a fixture tool
//! plugin learns a game plugin's session from `vosu.sessions`, receives
//! demand for its own namespace, publishes it through its own host
//! services for an API v1 subscriber, and its fields leave with it.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use vosu_core::api::{ApiPlan, SessionTarget};
use vosu_core::plugin::{self, PluginHost, PluginHostHandler, PluginInstanceId};
use vosu_core::state::{SessionKey, StateValue};
use vosu_plugin_api::{
    HostServices, Message, PublishBatchWriter, PublishValueRef, Selector, StateSubscribe,
    vosu_opcode, vosu_status,
};
use vosu_plugin_testkit::publisher::{self, PublisherEvent};

const WAIT: Duration = Duration::from_secs(10);
const OWNER: &str = "test.guest-e2e.game";
const TOOL: &str = "test.guest-e2e.tool";

struct Storage;
impl plugin::settings::SettingsStorage for Storage {
    fn load(
        &self,
        _: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, plugin::settings::SettingsError> {
        Ok(Default::default())
    }
    fn store(
        &self,
        _: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), plugin::settings::SettingsError> {
        Ok(())
    }
}

struct Console;
impl vosu_core::logging::ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
}

fn load(
    host: &Arc<PluginHost>,
    n: u64,
    id: &str,
    manifest: &str,
    schemas: &[&str],
) -> Receiver<PublisherEvent> {
    publisher::register_manifest(id, manifest);
    let events = publisher::register(id);
    let loaded = plugin::load_bundled(
        &semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap(),
        &plugin::ValidationSnapshot::default(),
        &[plugin::BundledPlugin {
            manifest,
            settings_schema: None,
            namespace_schemas: schemas,
            action_schemas: &[],
            vtable: publisher::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    host.create_instance(
        PluginInstanceId::new(n),
        &loaded[0],
        std::env::temp_dir(),
        Some(plugin::WorkerPool::new(2, 64)),
    )
    .unwrap();
    events
}

fn manifest(id: &str, rest: &str) -> String {
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='{id}'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\nconcurrent_dispatch=true\n{rest}"
    )
}

fn until<R>(
    events: &Receiver<PublisherEvent>,
    mut f: impl FnMut(PublisherEvent) -> Option<R>,
) -> R {
    let deadline = Instant::now() + WAIT;
    loop {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("bounded event deadline");
        if let Some(result) = f(event) {
            return result;
        }
    }
}

fn publish(id: &str, handle: u64, ns: &str, pointer: &str, value: u64) -> vosu_status {
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    writer.push(ns, pointer, PublishValueRef::U64(value));
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    publisher::host_services(id).send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes)
}

#[test]
fn non_owner_tool_publishes_its_own_namespace_for_api_v1_and_leaves_with_its_plugin() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let logs = tempfile::tempdir().unwrap();
    let logger = vosu_core::logging::Logger::new(
        logs.path(),
        vosu_core::logging::LoggerOptions {
            version: "test".into(),
            debug_log: true,
            queue_capacity: 64,
        },
        Arc::new(Console),
    )
    .unwrap();
    let host = PluginHost::new_with_foreground(
        logger,
        Arc::new(plugin::settings::PluginSettingsService::new(Arc::new(
            Storage,
        ))),
        runtime.handle().clone(),
        Arc::new(plugin::SyntheticForegroundSource::new(None)),
    );

    let game = load(
        &host,
        1,
        OWNER,
        &manifest(
            OWNER,
            &format!(
                "covers=['field:vosu.play.combo']\n[match]\nprocess_names=['{OWNER}']\n[[provides]]\nid='game:fixture'\nversion='*'\n"
            ),
        ),
        &[],
    );
    host.process_appeared(plugin::ProcessRecord {
        pid: 4801,
        name: OWNER.into(),
        path: format!("/games/{OWNER}"),
        command_line: OWNER.into(),
        argv: Some(vec![OWNER.into()]),
        bitness: vosu_plugin_api::Bitness::X64,
        start_time: 1,
    });
    let handle = until(&game, |event| match event {
        PublisherEvent::Attached { handle } => Some(handle),
        _ => None,
    });

    let schema = format!(
        r#"{{"title":"{TOOL}","version":"1.0.0","type":"object","properties":{{"timeline":{{"type":"integer","x-sampling-lane":{{"lane":"regular","activity":"always"}}}}}}}}"#
    );
    let tool = load(
        &host,
        2,
        TOOL,
        &manifest(
            TOOL,
            &format!("[[provides]]\nid='field:{TOOL}'\nversion='1.0.0'\nschema='state.json'\n"),
        ),
        &[&schema],
    );
    let path = format!("/{TOOL}/timeline");
    let plan = ApiPlan::new(&[&path]).unwrap();
    let api = host.api_state_source();
    let mut subscription = api.subscribe(SessionTarget::Active, plan.clone()).unwrap();

    assert_eq!(
        host.handle(
            PluginInstanceId::new(2),
            Message::StateSubscribe(StateSubscribe {
                session: None,
                selectors: vec![Selector {
                    ns: "vosu.sessions".into(),
                    pointer: String::new(),
                }],
            }),
            0,
        ),
        vosu_status::VOSU_OK
    );
    let demand = until(&tool, |event| match event {
        PublisherEvent::Demand(demand) => Some(demand),
        _ => None,
    });
    assert_eq!(demand.handle, handle);
    assert!(demand.regular && !demand.precise);
    assert_eq!(demand.selectors.len(), 1);
    assert_eq!(demand.selectors[0].ns, TOOL);

    assert_eq!(
        publish(TOOL, handle, TOOL, "/timeline", 12),
        vosu_status::VOSU_OK
    );
    let deadline = Instant::now() + WAIT;
    while subscription.updates.borrow().values[0] != Some(StateValue::U64(12)) {
        runtime
            .block_on(tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                subscription.changed(),
            ))
            .unwrap()
            .unwrap();
    }
    for (ns, pointer) in [("vosu.play", "/combo"), (OWNER, "/x")] {
        assert_eq!(
            publish(TOOL, handle, ns, pointer, 1),
            vosu_status::VOSU_ERR_PERMISSION_DENIED
        );
    }

    host.teardown_instance(PluginInstanceId::new(2)).unwrap();
    assert!(
        host.state()
            .store
            .current(SessionKey(handle))
            .unwrap()
            .namespace(TOOL)
            .is_none()
    );
    assert_eq!(
        api.read(&SessionTarget::Active, &plan).unwrap().values,
        vec![None]
    );
    drop(subscription);
    host.destroy_all(WAIT);
}
