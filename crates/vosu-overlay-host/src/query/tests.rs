use super::*;
use crate::model::SettingInput;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use vosu_plugin_api::manifest::action_schema::CompiledActionSchema;
use vosu_plugin_api::{Caller, vosu_status};

const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    root: tempfile::TempDir,
    counters: Arc<ReloadableCounterService>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("static")).unwrap();
        let counters = Arc::new(ReloadableCounterService::production(
            root.path().join("static"),
            root.path().join("config/settings"),
            root.path().join("cache"),
        ));
        Self { root, counters }
    }

    fn folder(&self, name: &str) -> PathBuf {
        let folder = self.root.path().join("static").join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("index.html"), "overlay").unwrap();
        folder
    }

    fn page(&self, after: Option<&str>, limit: usize) -> Value {
        let page = query_page(&self.counters, after, limit).unwrap();
        result_schema().validate_result(&page).unwrap();
        page
    }
}

fn result_schema() -> CompiledActionSchema {
    CompiledActionSchema::compile(crate::ACTION_SCHEMAS[1].1).unwrap()
}

fn ids(page: &Value) -> Vec<&str> {
    page["overlays"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect()
}

#[test]
fn entries_name_folders_and_entry_pages_never_paths() {
    let fixture = Fixture::new();
    let manifest = fixture.folder("manifest overlay");
    std::fs::write(
        manifest.join("manifest.toml"),
        format!(
            "manifest_version=1\nkind='overlay'\nid='manifest-overlay'\nname='Manifest overlay'\nversion='7.2.3'\nhost='={}'\n",
            crate::host_version()
        ),
    )
    .unwrap();
    let legacy = fixture.folder("legacy");
    std::fs::write(
        legacy.join("metadata.txt"),
        "Name: Legacy overlay\r\nVersion: 1.4\r\n",
    )
    .unwrap();
    fixture.folder("bare");
    fixture.folder("nested/inner");
    fixture
        .counters
        .set_name("legacy", Some("My legacy"), None)
        .unwrap();

    let page = fixture.page(None, DEFAULT_PAGE);
    assert_eq!(
        page,
        json!({
            "overlays": [
                {"id": "bare", "name": "bare", "entry": "/bare/", "version": null, "settings": {}},
                {"id": "legacy", "name": "My legacy", "entry": "/legacy/", "version": "1.4", "settings": {}},
                {"id": "manifest overlay", "name": "Manifest overlay", "entry": "/manifest%20overlay/", "version": "7.2.3", "settings": {}},
                {"id": "nested/inner", "name": "inner", "entry": "/nested/inner/", "version": null, "settings": {}},
            ],
            "next": null,
        })
    );
    let text = serde_json::to_string(&page).unwrap();
    for path in [
        fixture.root.path(),
        &fixture.root.path().join("static"),
        &fixture.root.path().join("config"),
    ] {
        assert!(!text.contains(&*path.to_string_lossy()), "{path:?} leaked");
    }
    assert!(!text.contains(&*std::env::temp_dir().to_string_lossy()));
}

#[test]
fn password_values_never_leave_the_host() {
    let fixture = Fixture::new();
    let folder = fixture.folder("secret");
    std::fs::write(
        folder.join("settings.json"),
        serde_json::to_vec(&json!([
            {"uniqueID": "token", "type": "password", "title": "Token", "value": "default-secret"},
            {"uniqueID": "heading", "type": "header", "title": "Heading", "value": "heading"},
            {"uniqueID": "label", "type": "text", "title": "Label", "value": "hello"},
            {"uniqueID": "size", "type": "number", "title": "Size", "value": "12"},
            {"uniqueID": "shown", "type": "checkbox", "title": "Shown", "value": "true"},
            {"uniqueID": "token", "type": "text", "title": "Reused id", "value": "reused"},
        ]))
        .unwrap(),
    )
    .unwrap();
    fixture
        .counters
        .settings_store()
        .save_user_values(
            "secret",
            &[
                SettingInput {
                    unique_id: "token".into(),
                    value: json!("saved-secret-value"),
                },
                SettingInput {
                    unique_id: "label".into(),
                    value: json!("saved label"),
                },
            ],
        )
        .unwrap();
    // The values file also keeps a key the schema no longer declares.
    let values = fixture
        .root
        .path()
        .join("config/settings/secret.settings.values.toml");
    let mut text = std::fs::read_to_string(&values).unwrap();
    text.push_str("\nstale = \"stale-secret\"\n");
    std::fs::write(&values, text).unwrap();

    let page = fixture.page(None, DEFAULT_PAGE);
    assert_eq!(
        page["overlays"][0]["settings"],
        json!({"label": "saved label", "size": 12.0, "shown": true})
    );
    let text = serde_json::to_string(&page).unwrap();
    for secret in [
        "saved-secret-value",
        "default-secret",
        "stale-secret",
        "token",
    ] {
        assert!(!text.contains(secret), "{secret} leaked");
    }
}

#[test]
fn pages_are_bounded_by_count_and_bytes_and_resume_after_the_cursor() {
    let fixture = Fixture::new();
    for index in 0..(MAX_PAGE + 10) {
        fixture.folder(&format!("overlay-{index:03}"));
    }
    let default = fixture.page(None, DEFAULT_PAGE);
    assert_eq!(ids(&default).len(), DEFAULT_PAGE);
    assert_eq!(default["next"], "overlay-019");

    let first = fixture.page(None, usize::MAX);
    assert_eq!(ids(&first).len(), MAX_PAGE);
    assert_eq!(first["next"], format!("overlay-{:03}", MAX_PAGE - 1));
    let second = fixture.page(first["next"].as_str(), usize::MAX);
    assert_eq!(ids(&second).len(), 10);
    assert_eq!(second["next"], Value::Null);
    let all: Vec<_> = ids(&first).into_iter().chain(ids(&second)).collect();
    let expected: Vec<_> = (0..(MAX_PAGE + 10))
        .map(|index| format!("overlay-{index:03}"))
        .collect();
    assert_eq!(all, expected);
    assert_eq!(ids(&fixture.page(None, 0)).len(), 1);
    assert_eq!(
        fixture.page(Some("overlay-999"), MAX_PAGE)["overlays"],
        json!([])
    );

    let args = CompiledActionSchema::compile(crate::ACTION_SCHEMAS[0].1).unwrap();
    args.validate_arguments(&json!({})).unwrap();
    args.validate_arguments(&json!({"after": "overlay-001", "limit": MAX_PAGE}))
        .unwrap();
    for refused in [
        json!({"limit": MAX_PAGE + 1}),
        json!({"limit": 0}),
        json!({"after": ""}),
        json!({"cursor": "x"}),
    ] {
        assert!(args.validate_arguments(&refused).is_err(), "{refused}");
    }
}

#[test]
fn large_settings_shrink_the_page_and_oversized_values_are_withheld() {
    let fixture = Fixture::new();
    let schema = json!([{"uniqueID": "notes", "type": "textarea", "title": "Notes", "value": ""}]);
    for index in 0..MAX_PAGE {
        let folder = fixture.folder(&format!("big-{index:02}"));
        std::fs::write(folder.join("settings.json"), schema.to_string()).unwrap();
        fixture
            .counters
            .settings_store()
            .save_user_values(
                &format!("big-{index:02}"),
                &[SettingInput {
                    unique_id: "notes".into(),
                    value: json!("n".repeat(SETTINGS_BUDGET - 64)),
                }],
            )
            .unwrap();
    }
    let huge = fixture.folder("huge");
    std::fs::write(huge.join("settings.json"), schema.to_string()).unwrap();
    fixture
        .counters
        .settings_store()
        .save_user_values(
            "huge",
            &[SettingInput {
                unique_id: "notes".into(),
                value: json!("h".repeat(SETTINGS_BUDGET)),
            }],
        )
        .unwrap();

    let mut cursor = None::<String>;
    let mut seen = Vec::new();
    loop {
        let page = fixture.page(cursor.as_deref(), MAX_PAGE);
        let bytes = serde_json::to_vec(&page["overlays"]).unwrap().len();
        assert!(bytes <= PAGE_BUDGET + 64, "{bytes} bytes");
        let entries = page["overlays"].as_array().unwrap();
        assert!(entries.len() < MAX_PAGE);
        for entry in entries {
            let id = entry["id"].as_str().unwrap();
            if id == "huge" {
                assert_eq!(entry["settings"], Value::Null);
            } else {
                assert_eq!(
                    entry["settings"]["notes"].as_str().map(str::len),
                    Some(SETTINGS_BUDGET - 64)
                );
            }
            seen.push(id.to_owned());
        }
        match page["next"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(seen.len(), MAX_PAGE + 1);
}

#[test]
fn the_list_follows_installs_and_uninstalls() {
    let fixture = Fixture::new();
    assert_eq!(fixture.page(None, DEFAULT_PAGE)["overlays"], json!([]));
    fixture.folder("alpha");
    fixture.folder("beta");
    assert_eq!(ids(&fixture.page(None, DEFAULT_PAGE)), ["alpha", "beta"]);
    fixture.counters.delete("alpha").unwrap();
    assert_eq!(ids(&fixture.page(None, DEFAULT_PAGE)), ["beta"]);
    fixture.folder("gamma");
    assert_eq!(ids(&fixture.page(None, DEFAULT_PAGE)), ["beta", "gamma"]);
}

#[derive(Clone)]
struct Recorder(mpsc::SyncSender<ActionResult>);

impl HostServices for Recorder {
    fn send(&self, message: &Message, _: u64) -> vosu_status {
        if let Message::ActionResult(result) = message {
            self.0.try_send(result.clone()).unwrap();
        }
        vosu_status::VOSU_OK
    }
}

fn recorder() -> (Recorder, Receiver<ActionResult>) {
    let (tx, rx) = mpsc::sync_channel(64);
    (Recorder(tx), rx)
}

fn invoke(invocation: u64, kind: CallerKind, action: &str, args: Value) -> ActionInvoke {
    ActionInvoke {
        invocation,
        action: action.into(),
        session: None,
        caller: Caller {
            kind,
            id: "acme.renderer".into(),
        },
        args: CborValue::serialized(&args).unwrap(),
    }
}

#[test]
fn the_instance_answers_plugins_once_and_refuses_every_other_caller() {
    let fixture = Fixture::new();
    fixture.folder("alpha");
    let data_dir = fixture.root.path().join("data/plugins/vosu.overlayhost");
    let (host, results) = recorder();
    let mut worker = QueryWorker::new(data_dir.clone());

    // Not yet bound: the instance exists before composition binds storage.
    worker
        .invoke(
            &host,
            invoke(1, CallerKind::Plugin, QUERY_ACTION, json!({})),
        )
        .unwrap();
    let unbound = results.recv_timeout(WAIT).unwrap();
    assert_eq!(
        (unbound.invocation, unbound.code.as_deref()),
        (1, Some("unavailable"))
    );

    let binding = bind_overlay_query(&data_dir, &fixture.counters);
    let other = bind_overlay_query(&fixture.root.path().join("elsewhere"), &fixture.counters);
    worker
        .invoke(
            &host,
            invoke(2, CallerKind::Plugin, QUERY_ACTION, json!({"limit": 5})),
        )
        .unwrap();
    let answered = results.recv_timeout(WAIT).unwrap();
    assert_eq!(answered.invocation, 2);
    assert_eq!(answered.status, ActionStatus::Ok);
    let value: Value = answered.value.unwrap().deserialized().unwrap();
    assert_eq!(ids(&value), ["alpha"]);

    for (invocation, kind, action, args, code) in [
        (
            3,
            CallerKind::Tool,
            QUERY_ACTION,
            json!({}),
            "permission_denied",
        ),
        (
            4,
            CallerKind::Cli,
            QUERY_ACTION,
            json!({}),
            "permission_denied",
        ),
        (
            5,
            CallerKind::Plugin,
            "vosu.overlayhost.other",
            json!({}),
            "unknown_action",
        ),
        (
            6,
            CallerKind::Plugin,
            QUERY_ACTION,
            json!({"cursor": 1}),
            "invalid_arguments",
        ),
    ] {
        worker
            .invoke(&host, invoke(invocation, kind, action, args))
            .unwrap();
        let refused = results.recv_timeout(WAIT).unwrap();
        assert_eq!(refused.invocation, invocation);
        assert_eq!(refused.status, ActionStatus::Error);
        assert_eq!(refused.code.as_deref(), Some(code));
        assert_eq!(refused.value, None);
    }

    drop(binding);
    worker
        .invoke(
            &host,
            invoke(7, CallerKind::Plugin, QUERY_ACTION, json!({})),
        )
        .unwrap();
    let unbound = results.recv_timeout(WAIT).unwrap();
    assert_eq!(unbound.code.as_deref(), Some("unavailable"));
    drop(worker);
    drop(other);
    assert!(results.try_recv().is_err());
}

#[test]
fn bundled_manifest_declares_the_query_action() {
    let manifest = vosu_plugin_api::manifest::PackageManifest::parse(crate::MANIFEST)
        .unwrap()
        .into_plugin()
        .unwrap();
    let [action] = manifest.actions.as_slice() else {
        panic!("one action")
    };
    assert_eq!(format!("{}.{}", manifest.id, action.name), QUERY_ACTION);
    assert_eq!(action.session, vosu_plugin_api::ActionSessionMode::None);
    assert!(!action.cli);
    assert_eq!(
        [action.args_schema.as_str(), action.result_schema.as_str()],
        [crate::ACTION_SCHEMAS[0].0, crate::ACTION_SCHEMAS[1].0]
    );
    assert!(manifest.provides.iter().any(|capability| capability.id
        == format!("action:{QUERY_ACTION}")
        && capability.version == action.version));
}
