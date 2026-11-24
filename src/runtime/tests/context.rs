use super::*;
#[path = "../../../tests/adapter_fixture/mod.rs"]
mod fixture;
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_osu_state::{Delivery, StateBundle, StateBundleSnapshot};

fn settings() -> InstanceSettings {
    InstanceSettings {
        tournament_spectator: false,
        tournament_ipc_id: None,
        devserver_endpoint: None,
    }
}
fn state(source: ClientType, root: &Path) -> Arc<StateBundleSnapshot> {
    let mut state = (*StateBundle::new(
        source,
        1,
        Arc::new(vosu_osu_state::state_backend::LiveStateCalculationBackend::new()),
    )
    .snapshot())
    .clone();
    let regular = Arc::make_mut(&mut state.regular);
    regular.global.game_folder = root.join("game").to_string_lossy().into_owned();
    regular.global.songs_folder = root.join("songs").to_string_lossy().into_owned();
    regular.global.skin_folder = "Selected Skin".into();
    regular.menu.folder = Some("chart".into());
    regular.menu.filename = Some("map.osu".into());
    regular.menu.audio_filename = Some("audio.mp3".into());
    regular.menu.background_filename = Some("bg.jpg".into());
    regular.menu.gamemode = Some(3);
    regular.menu.checksum = Some("selected-1973".into());
    Arc::make_mut(&mut regular.settings.values)
        .insert("client".into(), serde_json::json!({"version":"2026.714.0"}));
    Arc::new(state)
}
fn until(predicate: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            std::time::Instant::now() < deadline,
            "context readiness deadline"
        );
        std::thread::yield_now();
    }
}
#[test]
fn selected_context_confines_files_and_preserves_stable_lazer_display_contract() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        let mut fixture = fixture::Fixture::new(source);
        let root = fixture.h.root.path();
        std::fs::create_dir_all(root.join("game/Skins/Selected Skin")).unwrap();
        std::fs::create_dir_all(root.join("songs/chart")).unwrap();
        for file in ["map.osu", "audio.mp3", "bg.jpg"] {
            std::fs::write(root.join("songs/chart").join(file), file).unwrap();
        }
        assert!(tool_context(&fixture.files, &fixture.h.host)["skin"].is_null());
        let snapshot = state(source, root);
        let game = root.join("game");
        let songs = root.join("songs");
        let publisher = fixture.attach(settings(), false, true);
        publisher.publish(snapshot.clone(), Delivery::All).unwrap();
        until(|| fixture.files.tool_context().is_ok());
        if source == ClientType::Stable {
            until(|| {
                tool_context(&fixture.files, &fixture.h.host)["beatmap"]["checksum"]
                    == "selected-1973"
            });
            let context = tool_context(&fixture.files, &fixture.h.host);
            assert_eq!(context["osu_dir"], serde_json::json!(game));
            assert_eq!(
                context["skin"]["folder"],
                serde_json::json!(game.join("Skins/Selected Skin"))
            );
            assert_eq!(
                context["songs_dir"],
                serde_json::json!(songs.canonicalize().unwrap())
            );
            assert_eq!(
                context["beatmap"]["file"],
                serde_json::json!(songs.join("chart/map.osu").canonicalize().unwrap())
            );
            assert_eq!(context["beatmap"]["mode"], "mania");
            assert!(context["skin"]["files"].as_bool().unwrap());
            let other = fixture.attach(settings(), false, false);
            let mut unfocused = (*snapshot).clone();
            Arc::make_mut(&mut unfocused.regular).menu.checksum = Some("foreign-6881".into());
            other.publish(Arc::new(unfocused), Delivery::All).unwrap();
            assert_eq!(
                tool_context(&fixture.files, &fixture.h.host)["beatmap"]["checksum"],
                "selected-1973"
            );
            for path in ["missing.osu", "../../outside.osu"] {
                let mut next = (*snapshot).clone();
                Arc::make_mut(&mut next.regular).menu.filename = Some(path.into());
                publisher.publish(Arc::new(next), Delivery::All).unwrap();
                until(|| {
                    fixture.files.tool_context().is_ok_and(|context| {
                        context.file.as_deref() == Some(&format!("chart/{path}"))
                    })
                });
                assert!(tool_context(&fixture.files, &fixture.h.host)["beatmap"].is_null());
            }
            for game in ["", "relative-game"] {
                let mut next = (*snapshot).clone();
                let marker = format!("invalid-root-{game}");
                let regular = Arc::make_mut(&mut next.regular);
                regular.global.game_folder = game.into();
                regular.menu.checksum = Some(marker.clone());
                publisher.publish(Arc::new(next), Delivery::All).unwrap();
                until(|| {
                    fixture
                        .files
                        .tool_context()
                        .is_ok_and(|context| context.checksum.as_deref() == Some(marker.as_str()))
                });
                assert!(tool_context(&fixture.files, &fixture.h.host)["osu_dir"].is_null());
            }
        } else {
            let context = tool_context(&fixture.files, &fixture.h.host);
            assert_eq!(context["client"], "lazer");
            assert!(context["osu_dir"].is_null());
            assert!(context["skin"]["folder"].is_null());
        }
    }
}
#[test]
fn dashboard_and_report_keep_shape_and_host_lifecycle_without_a_phase_task() {
    use crate::server::service::ReportInputProvider;
    let mut fixture = fixture::Fixture::new(ClientType::Stable);
    let publisher = fixture.attach(settings(), false, true);
    let config = Arc::new(
        fixture
            .h
            .runtime
            .block_on(ConfigStore::initialize(
                fixture.h.root.path().join("vosu.toml"),
            ))
            .unwrap(),
    );
    let root = fixture.h.root.path();
    std::fs::create_dir_all(root.join("static")).unwrap();
    let report = RuntimeReportInput {
        config: config.clone(),
        plugins: fixture.h.host.clone(),
        files: fixture.files.clone(),
        counters: Arc::new(ReloadableCounterService::production(
            root.join("static"),
            root.join("settings"),
            root.join("cache"),
        )),
        logs: root.join("logs"),
    };
    assert_eq!(
        report.dashboard_status()["instances"][0]["state"],
        "starting reader"
    );
    publisher
        .publish(state(ClientType::Stable, root), Delivery::All)
        .unwrap();
    until(|| report.dashboard_status()["instances"][0]["version"] == "2026.714.0");
    let status = report.dashboard_status();
    let instance = &status["instances"][0];
    assert_eq!(instance.as_object().unwrap().len(), 6);
    for field in [
        "pid",
        "client",
        "version",
        "state",
        "pollIntervalMs",
        "precisePollIntervalMs",
    ] {
        assert!(instance.get(field).is_some(), "{field}");
    }
    assert_eq!(instance["client"], "stable");
    assert_eq!(instance["state"], "reading");
    let capture = report.capture().unwrap();
    assert_eq!(capture.instances[0].version, "2026.714.0");
    assert_eq!(capture.instances[0].bitness, "x86");
    fixture.h.runtime.block_on(config.shutdown()).unwrap();
}
