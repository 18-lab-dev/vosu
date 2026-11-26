use super::*;
use serde_json::{Value, json};
use vosu_proptest::bounded;

/// A 1.x package: listed as `needs-2.0-update`, never parsed or run.
fn put(root: &Path, id: &str) {
    fs::create_dir_all(root.join("web")).unwrap();
    fs::write(
        root.join("tool.toml"),
        format!("manifest_version=1\nid='{id}'\nname='{id}'\nversion='1.0.0'\n[ui]\nroot='web'\n"),
    )
    .unwrap();
    fs::write(root.join("web/index.html"), id).unwrap();
}
async fn host(root: &Path) -> Arc<ToolRegistry> {
    bounded!(ToolRegistry::new(
        root.join("config"),
        root.join("tools"),
        root.join("cache"),
        ExtraRoots::default(),
        "http://127.0.0.1:25150".into(),
        Some("http://127.0.0.1:25151".into()),
        semver::Version::parse(FIXTURE_VERSION).unwrap(),
    ))
    .unwrap()
}
async fn fixture(root: &Path) -> Arc<ToolRegistry> {
    put(&root.join("tools/old"), "old");
    let state = Saved {
        enabled: Some(false),
        trusted: true,
        catalog_source: Some("fixture/source".into()),
        autostart: Some(false),
        ..Saved::default()
    };
    atomic_json(
        &root.join("config/state.json"),
        &BTreeMap::from([("old", state)]),
    )
    .unwrap();
    let host = bounded!(host(root));
    fs::create_dir_all(host.tool_data("old")).unwrap();
    fs::write(host.tool_data("old").join("save"), b"unchanged\0data").unwrap();
    fs::create_dir_all(host.cache.join("old")).unwrap();
    fs::write(host.cache.join("old/item"), "cached").unwrap();
    atomic_json(
        &host.tool_settings("old"),
        &json!({"secret":"retain me", "count":42}),
    )
    .unwrap();
    host
}

/// The durable intent a 1.x catalog rename published before moving anything.
fn journal(host: &ToolRegistry) {
    let mut state = host.saved.lock().get("old").cloned().unwrap();
    state.replaces.push("old".into());
    atomic_json(
        &host.replacement_journal(),
        &Replacement {
            old: "old".into(),
            new: "new".into(),
            state,
            data: true,
            settings: true,
            cache: true,
        },
    )
    .unwrap();
}

#[tokio::test]
async fn every_crash_boundary_recovers_data_settings_and_state_idempotently() {
    for cut in 0..8 {
        let root = tempfile::tempdir().unwrap();
        let original = bounded!(fixture(root.path()));
        put(&original.replacement_stage(), "new");
        journal(&original);
        let paths = [
            (original.replacement_stage(), original.data.join("new")),
            (original.tool_data("old"), original.tool_data("new")),
            (original.tool_settings("old"), original.tool_settings("new")),
            (original.cache.join("old"), original.cache.join("new")),
        ];
        for (from, to) in paths.iter().take(cut.min(4)) {
            move_once(from, to, true).unwrap();
        }
        if cut >= 5 {
            let tx: Replacement =
                serde_json::from_slice(&fs::read(original.replacement_journal()).unwrap()).unwrap();
            atomic_json(
                &original.config.join("state.json"),
                &BTreeMap::from([("new", tx.state)]),
            )
            .unwrap();
        }
        if cut >= 6 {
            fs::remove_file(original.data.join("old/web/index.html")).unwrap();
        }
        if cut >= 7 {
            fs::remove_dir_all(original.data.join("old")).unwrap();
            fs::remove_dir(original.data.join(".replacement-stage")).unwrap();
        }
        bounded!(original.shutdown());
        drop(original);
        let recovered = bounded!(host(root.path()));
        recovered.recover_replacement().unwrap();
        recovered.recover_replacement().unwrap();
        assert!(recovered.entry("old").is_err());
        assert!(recovered.entry("new").is_ok());
        assert!(!recovered.data.join("old").exists());
        assert!(!recovered.replacement_journal().exists());
        assert!(!recovered.data.join(".replacement-stage").exists());
        assert_eq!(
            fs::read(recovered.tool_data("new").join("save")).unwrap(),
            b"unchanged\0data"
        );
        assert_eq!(
            fs::read_to_string(recovered.cache.join("new/item")).unwrap(),
            "cached"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(recovered.tool_settings("new")).unwrap())
                .unwrap(),
            json!({"secret":"retain me", "count":42})
        );
        assert!(!recovered.tool_data("old").exists());
        assert!(!recovered.tool_settings("old").exists());
        let state = recovered.saved.lock().get("new").cloned().unwrap();
        assert_eq!(state.enabled, Some(false));
        assert_eq!(state.autostart, Some(false));
        assert!(state.trusted);
        assert_eq!(state.catalog_source.as_deref(), Some("fixture/source"));
        assert!(recovered.saved.lock().get("old").is_none());
        // The recovered 1.x package is still a legacy entry: listed with its
        // retained trust, never presented or launchable.
        let view = recovered.tool_view(&recovered.entry("new").unwrap());
        assert_eq!(view["status_detail"], NEEDS_UPDATE);
        assert_eq!(view["trusted"], true);
        assert!(recovered.tool_host_views().borrow().tools.is_empty());
        bounded!(recovered.shutdown());
    }
}

#[test]
fn native_rename_never_clobbers_a_file_or_empty_directory() {
    for directory in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        let to = root.path().join("to");
        if directory {
            fs::create_dir(&from).unwrap();
            fs::create_dir(&to).unwrap();
        } else {
            fs::write(&from, "source").unwrap();
            fs::write(&to, "destination").unwrap();
        }
        assert!(rename_new(&from, &to).is_err());
        assert!(from.exists());
        assert!(to.exists());
        if !directory {
            assert_eq!(fs::read_to_string(to).unwrap(), "destination");
        }
    }
}
