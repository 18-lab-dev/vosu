//! I1: production registry/schema -> J2 HTTP -> J1 executor -> real osu plugin.
#![expect(
    clippy::disallowed_methods,
    reason = "isolated temporary import archives and selected Songs fixtures"
)]
use super::support::{Harness, bounded, record};
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{Method, StatusCode},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use vosu::server::api_v1::{
    actions::{ActionAuthority, ActionRoutes},
    router::ApiV1Router,
    subscriptions::ApiV1SubscriptionHub,
};
use vosu_core::{
    actions::{ActionDescriptor, ActionRegistry, JobOwner, JobRegistry, Principal, ToolAuthority},
    identity::PluginInstanceId,
    plugin,
};
struct Authority(JobOwner);
impl ToolAuthority for Authority {
    fn authorizes(&self, owner: &JobOwner, action: &ActionDescriptor) -> bool {
        owner.same(&self.0) && action.id == vosu_osu::actions::IMPORT_ACTION
    }
}
impl ActionAuthority for Authority {
    fn authenticate_action_session(&self, token: &str) -> Option<Principal> {
        (token == "i1-owned-launch").then(|| Principal::Tool(self.0.clone()))
    }
    fn authorize_action_session(&self, token: &str, action: &str) -> Option<Principal> {
        (action == vosu_osu::actions::IMPORT_ACTION)
            .then(|| self.authenticate_action_session(token))
            .flatten()
    }
}
fn request(method: Method, path: &str, arguments: Value) -> Request {
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:25050")
        .header("x-vosu-tool-session", "i1-owned-launch")
        .body(Body::from(json!({"arguments":arguments}).to_string()))
        .unwrap()
}
async fn body(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &bounded(to_bytes(response.into_body(), 1024 * 1024))
            .await
            .unwrap(),
    )
    .unwrap()
}
#[test]
fn real_osu_import_action_http_registry_schema_and_terminal() {
    let h = Harness::new();
    let results = super::import_fixture::register(h.root.path().into());
    let production = &vosu::plugins::BUNDLED[0];
    let loaded = plugin::load_bundled(&[plugin::BundledPlugin {
        manifest: production.manifest,
        settings_schema: production.settings_schema,
        namespace_schemas: production.namespace_schemas,
        action_schemas: production.action_schemas,
        vtable: super::import_fixture::vosu_plugin_vtable,
    }])
    .unwrap();
    h.host
        .create_instance(
            PluginInstanceId::new(917310),
            &loaded[0],
            h.root.path().into(),
            Some(plugin::WorkerPool::new(2, 64)),
        )
        .unwrap();
    let descriptor = h.host.get(vosu_osu::actions::IMPORT_ACTION).unwrap();
    assert!(descriptor.cli);
    assert_eq!(
        descriptor.session,
        vosu_plugin_api::ActionSessionMode::Required
    );
    assert!(h.host.get("vosu.osu.beatmap.export").is_none());
    assert_eq!(descriptor.version, semver::Version::new(1, 0, 0));
    let mut process = record(917311, "osu!.exe");
    process.path = h
        .root
        .path()
        .join("osu!.exe")
        .to_string_lossy()
        .into_owned();
    process.argv = Some(vec![process.path.clone()]);
    process.command_line = process.path.clone();
    process.bitness = vosu_plugin_api::Bitness::X86;
    h.host.process_appeared(process);
    h.runtime.block_on(bounded(async {
        while h
            .host
            .api_state_source()
            .session(&vosu_core::api::SessionTarget::Active)
            .is_err()
        {
            tokio::task::yield_now().await;
        }
        let source = h.host.api_state_source();
        let hub = ApiV1SubscriptionHub::new(source.clone(), Duration::ZERO);
        let api = ApiV1Router::new(source, hub.clone()).with_actions(ActionRoutes::new(
            h.host.clone(),
            h.host.invocation_executor(),
            JobRegistry::new(),
            Arc::new(Authority(JobOwner::for_tool("i1-tool".into()))),
        ));
        let action = vosu_osu::actions::IMPORT_ACTION;
        let absent = h.root.path().join("private-missing-source-sentinel.osz");
        let args = json!({"sourcePath":absent});
        assert_eq!(
            api.handle(request(
                Method::POST,
                &format!("/api/v1/actions/{action}"),
                args.clone()
            ))
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        let path = format!("/api/v1/sessions/active/actions/{action}?wait=5000");
        let invalid = api
            .handle(request(Method::POST, &path, json!({"sourcePath":13})))
            .await;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body(invalid).await["error"]["code"], "invalid_arguments");
        let response = api.handle(request(Method::POST, &path, args)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let result = body(response).await;
        assert_eq!(result["state"], "failed");
        assert_eq!(result["error"]["code"], "invalid_archive");
        assert!(
            !result
                .to_string()
                .contains("private-missing-source-sentinel")
        );
        let songs = h.root.path().join("selected Songs");
        let other = h.root.path().join("unselected Songs");
        std::fs::create_dir(&songs).unwrap();
        std::fs::create_dir(&other).unwrap();
        let existing = songs.join("private-import-sentinel.osz");
        std::fs::write(&existing, b"existing no-clobber sentinel").unwrap();
        assert!(vosu_osu::test_support::observe_resource_snapshot(
            917311,
            &resource_snapshot(&songs)
        ));
        let source = import_archive(h.root.path());
        let original = std::fs::read(&source).unwrap();
        let response = api
            .handle(request(Method::POST, &path, json!({"sourcePath":source})))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let terminal = body(response).await;
        assert_eq!(terminal["state"], "succeeded");
        assert_eq!(
            terminal["result"],
            json!({"client":"stable","disposition":"queued","refreshRequired":true})
        );
        // Three progress messages plus exactly one accepted terminal result.
        assert_eq!(terminal["revision"], 4);
        let files: Vec<_> = std::fs::read_dir(&songs)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(files.len(), 2);
        let committed = files.iter().find(|p| **p != existing).unwrap();
        assert_eq!(committed.extension().unwrap(), "osz");
        assert_eq!(std::fs::read(committed).unwrap(), original);
        assert_eq!(
            std::fs::read(&existing).unwrap(),
            b"existing no-clobber sentinel"
        );
        assert_eq!(std::fs::read_dir(&other).unwrap().count(), 0);
        let job_path = format!("/api/v1/jobs/{}", terminal["id"].as_str().unwrap());
        assert_eq!(
            api.handle(request(Method::DELETE, &job_path, Value::Null))
                .await
                .status(),
            StatusCode::OK
        );
        // Join the production import worker before proving no later result or
        // cancellation can create a second terminal revision/artifact.
        let host = h.host.clone();
        bounded(tokio::task::spawn_blocking(move || {
            host.destroy_all(Duration::from_secs(5))
        }))
        .await
        .unwrap();
        assert_eq!(
            body(
                api.handle(request(Method::GET, &job_path, Value::Null))
                    .await
            )
            .await,
            terminal
        );
        assert_eq!(std::fs::read_dir(&songs).unwrap().count(), 2);
        let emitted = results.lock().unwrap();
        assert_eq!(
            emitted.len(),
            2,
            "one failed archive plus one successful import"
        );
        assert_eq!(
            emitted
                .iter()
                .filter(|r| r.status == vosu_plugin_api::ActionStatus::Ok)
                .count(),
            1
        );
        assert_ne!(emitted[0].invocation, emitted[1].invocation);
        assert_eq!(emitted[0].code.as_deref(), Some("invalid_archive"));
        assert_eq!(
            emitted[1]
                .value
                .as_ref()
                .unwrap()
                .deserialized::<Value>()
                .unwrap(),
            terminal["result"]
        );
        hub.shutdown();
    }));
}

fn import_archive(root: &std::path::Path) -> std::path::PathBuf {
    use std::io::Write;
    let source = root.join("private-import-sentinel.osz");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
    zip.start_file("chart.osu", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"osu file format v14\n\n[General]\nMode:0\n\n[Metadata]\nTitle:J3\nArtist:Sentinel\nCreator:Fixture\nVersion:Import\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:6\nApproachRate:7\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,1,100,1,0\n\n[HitObjects]\n256,192,1000,1,0,0:0:0:0:\n").unwrap();
    zip.finish().unwrap();
    source
}

fn resource_snapshot(songs: &std::path::Path) -> vosu_osu_state::StateBundleSnapshot {
    use vosu_osu_state::*;
    StateBundleSnapshot {
        source: vosu_osu_model::enums::ClientType::Stable,
        generation: 783331,
        reset_epoch: 0,
        regular: Arc::new(RegularState {
            global: GlobalState {
                songs_folder: songs.to_string_lossy().into_owned(),
                ..Default::default()
            },
            settings: Default::default(),
            beatmap: Default::default(),
            menu: Default::default(),
            bass_density: Default::default(),
            gameplay: Default::default(),
            result_screen: Default::default(),
            tourney: Default::default(),
            user: Default::default(),
            lazer_multi_spectating: Default::default(),
            ranked_play: Default::default(),
            room: Default::default(),
        }),
        precise: Arc::new(PreciseState {
            catcher: None,
            status: 0,
            play_time: 0.0,
            mode: 0,
            key_overlay: Arc::new(vec![]),
            keypresses: Arc::new(vec![]),
            hit_errors: Arc::new(Default::default()),
            total_hit_errors: 0.0,
            committed_hit_error_index: 0,
            unstable_rate: 0.0,
        }),
    }
}
