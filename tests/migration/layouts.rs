//! Native OS filesystem path layouts (normal, portable, spaces/Unicode),
//! bidirectional Release vs Prerelease channel isolation, and non-daemon CLI
//! non-migration invariants (M8-X01, M8-X02, M8-V02).

use std::fs;
use std::process::Command;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use vosu_core::catalog::sources::SourceStore;
use vosu_core::migration::{
    MigrationOutcome, OSU_PLUGIN_ID, OVERLAY_HOST_PLUGIN_ID, OVERLAY_HOST_SETTING_KEY, journal_path,
};
use vosu_core::plugin::settings::SettingsStorage;
use vosu_core::tools::{ExtraRoots, ToolRegistry};
use vosu_proptest::bounded;
use vosu_sys::platform::{BuildChannel, Paths};

use crate::support::{
    NoopSourceLifecycle, TestPathResolver, pre_rename_env_filename, read_toml_table,
    run_prerelease_startup, run_release_startup, seed_realistic_1x_fixture, snapshot_tree,
};

#[tokio::test]
async fn normal_and_portable_native_os_layouts_migrate_cleanly() {
    // 1. Native OS resolved normal layout (`Paths::resolve(..., Release)`).
    let normal_tmp = tempfile::tempdir().unwrap();
    let program_dir = normal_tmp.path().join("program");
    let launch_dir = normal_tmp.path().join("launch");
    fs::create_dir_all(&program_dir).unwrap();
    fs::create_dir_all(&launch_dir).unwrap();

    let exe_name = if cfg!(windows) { "vosu.exe" } else { "vosu" };
    let resolver = TestPathResolver {
        executable: program_dir.join(exe_name),
        launch: launch_dir,
        root: normal_tmp.path().to_path_buf(),
    };
    let paths = Paths::resolve(&resolver, BuildChannel::Release).unwrap();
    paths.initialize().unwrap();

    #[cfg(target_os = "linux")]
    {
        assert_eq!(paths.config(), normal_tmp.path().join("xdg-config/vosu"));
        assert_eq!(paths.data(), normal_tmp.path().join("xdg-data/vosu"));
        assert_eq!(paths.cache(), normal_tmp.path().join("xdg-cache/vosu"));
    }
    #[cfg(windows)]
    {
        assert_eq!(paths.config(), program_dir);
        assert_eq!(paths.data(), program_dir);
        assert_eq!(paths.cache(), program_dir.join(".cache"));
    }

    seed_realistic_1x_fixture(paths.config(), paths.data());
    let (outcome, cfg, settings) = run_release_startup(paths.config(), paths.data())
        .await
        .unwrap();
    assert!(matches!(outcome, MigrationOutcome::Completed(_)));
    assert_eq!(
        settings.load(OSU_PLUGIN_ID).unwrap().get("POLL_RATE"),
        Some(&json!(240.0))
    );
    assert!(journal_path(paths.config()).is_file());
    bounded!(cfg.shutdown()).unwrap();

    // Second run on normal layout is AlreadyComplete.
    let (second, cfg2, _) = run_release_startup(paths.config(), paths.data())
        .await
        .unwrap();
    assert!(matches!(second, MigrationOutcome::AlreadyComplete(_)));
    bounded!(cfg2.shutdown()).unwrap();

    // 2. Portable co-located layout (config_root == data_root in a single folder).
    let portable_tmp = tempfile::tempdir().unwrap();
    let portable_root = portable_tmp.path().join("vosu-portable");
    fs::create_dir_all(&portable_root).unwrap();
    seed_realistic_1x_fixture(&portable_root, &portable_root);

    let (portable_outcome, portable_cfg, portable_settings) =
        run_release_startup(&portable_root, &portable_root)
            .await
            .unwrap();
    assert!(matches!(portable_outcome, MigrationOutcome::Completed(_)));
    assert_eq!(
        portable_settings
            .load(OSU_PLUGIN_ID)
            .unwrap()
            .get("POLL_RATE"),
        Some(&json!(240.0))
    );
    assert_eq!(
        portable_settings
            .load(OVERLAY_HOST_PLUGIN_ID)
            .unwrap()
            .get(OVERLAY_HOST_SETTING_KEY),
        Some(&json!("custom-overlays"))
    );
    assert!(!portable_root.join("vosu.env").exists());
    assert!(!portable_root.join(pre_rename_env_filename()).exists());
    bounded!(portable_cfg.shutdown()).unwrap();
}

#[tokio::test]
async fn paths_with_spaces_and_unicode_migrate_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join("vosu テスト café 1.x");
    let config = base.join("config root 日本語");
    let data = base.join("data root ümlaut");
    let cache = base.join("cache root");
    fs::create_dir_all(&cache).unwrap();

    seed_realistic_1x_fixture(&config, &data);

    // Override STATIC_FOLDER_PATH with a path containing spaces & Unicode.
    let unicode_static = base.join("overlays フォルダー café");
    fs::create_dir_all(&unicode_static).unwrap();
    let mut toml_text = fs::read_to_string(config.join("vosu.toml")).unwrap();
    toml_text = toml_text.replace(
        "STATIC_FOLDER_PATH = \"custom-overlays\"",
        &format!(
            "STATIC_FOLDER_PATH = {}",
            toml::Value::String(unicode_static.to_string_lossy().into_owned())
        ),
    );
    fs::write(config.join("vosu.toml"), toml_text).unwrap();

    let (outcome, cfg, settings) = run_release_startup(&config, &data).await.unwrap();
    assert!(matches!(outcome, MigrationOutcome::Completed(_)));
    assert_eq!(
        settings
            .load(OVERLAY_HOST_PLUGIN_ID)
            .unwrap()
            .get(OVERLAY_HOST_SETTING_KEY),
        Some(&json!(unicode_static.to_string_lossy().into_owned()))
    );

    let sources = SourceStore::open(&config, Arc::new(NoopSourceLifecycle)).unwrap();
    assert_eq!(sources.snapshot().unwrap().1.len(), 6);

    let registry = bounded!(ToolRegistry::new(
        config.join("tools"),
        data.join("tools"),
        cache.join("tools"),
        ExtraRoots::default(),
        "http://127.0.0.1:24123".to_owned(),
        Some("http://127.0.0.1:24124".to_owned()),
        semver::Version::new(2, 0, 0),
    ))
    .unwrap();
    bounded!(registry.shutdown());
    bounded!(cfg.shutdown()).unwrap();

    // Re-running under the Unicode/spaces root is a clean no-op.
    let (second, cfg2, _) = run_release_startup(&config, &data).await.unwrap();
    assert!(matches!(second, MigrationOutcome::AlreadyComplete(_)));
    bounded!(cfg2.shutdown()).unwrap();
}

#[tokio::test]
async fn prerelease_and_release_channel_isolation_is_strictly_bidirectional() {
    let tmp = tempfile::tempdir().unwrap();
    let program_dir = tmp.path().join("program");
    let launch_dir = tmp.path().join("launch");
    fs::create_dir_all(&program_dir).unwrap();
    fs::create_dir_all(&launch_dir).unwrap();

    let exe_name = if cfg!(windows) { "vosu.exe" } else { "vosu" };
    let resolver = TestPathResolver {
        executable: program_dir.join(exe_name),
        launch: launch_dir,
        root: tmp.path().to_path_buf(),
    };
    let normal_paths = Paths::resolve(&resolver, BuildChannel::Release).unwrap();
    let alpha_paths = Paths::resolve(&resolver, BuildChannel::Prerelease).unwrap();
    normal_paths.initialize().unwrap();
    alpha_paths.initialize().unwrap();

    assert_eq!(
        alpha_paths.config(),
        normal_paths.config().join("prerelease")
    );
    assert_eq!(alpha_paths.data(), normal_paths.data().join("prerelease"));

    // Seed normal root with realistic 1.x files plus a normal-root broker credential.
    seed_realistic_1x_fixture(normal_paths.config(), normal_paths.data());
    fs::write(
        normal_paths.config().join("broker.json"),
        r#"{"token":"normal-secret"}"#,
    )
    .unwrap();

    let normal_config_before = snapshot_tree(normal_paths.config(), Some("prerelease"));
    let normal_data_before = snapshot_tree(normal_paths.data(), Some("prerelease"));

    // Phase 1: Prerelease startup imports read-only into `<root>/prerelease/`,
    // NEVER writes `<normal>/migration-v2-journal.json`, and NEVER mutates normal files.
    let (pre_outcome, pre_cfg, pre_settings) =
        run_prerelease_startup(&alpha_paths, normal_paths.config())
            .await
            .unwrap();
    assert_eq!(pre_outcome, MigrationOutcome::SkippedPrerelease);
    assert_eq!(pre_cfg.current().server_port, 24_123.0);
    assert!(!journal_path(normal_paths.config()).exists());
    assert!(!journal_path(alpha_paths.config()).exists());
    assert_eq!(
        snapshot_tree(normal_paths.config(), Some("prerelease")),
        normal_config_before,
        "prerelease startup mutated normal config root"
    );
    assert_eq!(
        snapshot_tree(normal_paths.data(), Some("prerelease")),
        normal_data_before,
        "prerelease startup mutated normal data root"
    );
    bounded!(pre_cfg.shutdown()).unwrap();

    // Phase 2: Populate `<root>/prerelease/` with distinct alpha credentials,
    // settings, config, catalog sources, and tool state.
    fs::write(
        alpha_paths.config().join("broker.json"),
        r#"{"token":"alpha-only-secret-must-never-import"}"#,
    )
    .unwrap();
    fs::write(
        alpha_paths.config().join("vosu.toml"),
        "SERVER_PORT = 29876.0\nALLOWED_IPS = \"alpha-only\"\n",
    )
    .unwrap();
    pre_settings
        .store(
            OSU_PLUGIN_ID,
            &Map::from_iter([
                ("POLL_RATE".to_owned(), json!(987.0)),
                ("CALCULATE_PP".to_owned(), Value::Bool(true)),
            ]),
        )
        .unwrap();
    fs::create_dir_all(alpha_paths.config().join("tools")).unwrap();
    fs::write(
        alpha_paths.config().join("tools/state.json"),
        r#"{"alpha-only-tool":{"enabled":true,"trusted":true}}"#,
    )
    .unwrap();

    let alpha_config_before = snapshot_tree(alpha_paths.config(), None);
    let alpha_data_before = snapshot_tree(alpha_paths.data(), None);

    // Phase 3: Normal `BuildChannel::Release` startup migrates ONLY normal root,
    // NEVER imports prerelease credentials/settings/state, and NEVER mutates `<root>/prerelease/`.
    let (rel_outcome, rel_cfg, rel_settings) =
        run_release_startup(normal_paths.config(), normal_paths.data())
            .await
            .unwrap();
    assert!(matches!(rel_outcome, MigrationOutcome::Completed(_)));
    assert_eq!(rel_cfg.current().server_port, 24_123.0);
    assert_ne!(rel_cfg.current().server_port, 29_876.0);
    assert_eq!(
        rel_settings.load(OSU_PLUGIN_ID).unwrap().get("POLL_RATE"),
        Some(&json!(240.0))
    );
    assert_eq!(
        rel_settings
            .load(OSU_PLUGIN_ID)
            .unwrap()
            .get("CALCULATE_PP"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        fs::read_to_string(normal_paths.config().join("broker.json")).unwrap(),
        r#"{"token":"normal-secret"}"#
    );
    let normal_table = read_toml_table(&normal_paths.config().join("vosu.toml"));
    assert_ne!(
        normal_table
            .get("ALLOWED_IPS")
            .and_then(toml::Value::as_str),
        Some("alpha-only")
    );

    // Entire `<root>/prerelease/` subtree remained byte-for-byte untouched!
    assert_eq!(
        snapshot_tree(alpha_paths.config(), None),
        alpha_config_before,
        "normal release migration modified prerelease config directory"
    );
    assert_eq!(
        snapshot_tree(alpha_paths.data(), None),
        alpha_data_before,
        "normal release migration modified prerelease data directory"
    );
    bounded!(rel_cfg.shutdown()).unwrap();
}

#[test]
fn non_daemon_cli_subcommands_never_trigger_normal_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let config_root = tmp.path().join("xdg-config");
    let data_root = tmp.path().join("xdg-data");
    let cache_root = tmp.path().join("xdg-cache");
    let normal_cfg = config_root.join("vosu");
    let normal_data = data_root.join("vosu");
    fs::create_dir_all(&cache_root).unwrap();
    seed_realistic_1x_fixture(&normal_cfg, &normal_data);

    let cfg_before = snapshot_tree(&normal_cfg, None);
    let data_before = snapshot_tree(&normal_data, None);

    for arg in ["--help", "--version"] {
        let output = Command::new(env!("CARGO_BIN_EXE_vosu"))
            .arg(arg)
            .env("XDG_CONFIG_HOME", &config_root)
            .env("XDG_DATA_HOME", &data_root)
            .env("XDG_CACHE_HOME", &cache_root)
            .env("HOME", tmp.path())
            .output()
            .expect("spawn vosu CLI");
        assert!(
            output.status.success(),
            "vosu {arg} exited with {:?}",
            output.status
        );
    }

    assert!(!journal_path(&normal_cfg).exists());
    assert_eq!(snapshot_tree(&normal_cfg, None), cfg_before);
    assert_eq!(snapshot_tree(&normal_data, None), data_before);
}
