use super::*;

fn put(root: &Path, id: &str, requires: &str) {
    fs::create_dir_all(root.join("web")).unwrap();
    fs::write(root.join("tool.toml"), format!("manifest_version=1\nid='{id}'\nname='{id}'\nversion='1.0.0'\n[ui]\nroot='web'\n{requires}")).unwrap();
    fs::write(root.join("web/index.html"), id).unwrap();
}
fn tool() -> catalog::Tool {
    serde_json::from_value(json!({"id":"new", "name":"New", "version":"1.0.0",
        "description":"fixture", "author":"test", "protocol":1, "interfaces":["ui"],
        "replaces":["old"], "packages":{"any":{"url":"https://example.com/new.zip", "sha256":"00".repeat(32), "size":1}}})).unwrap()
}
async fn host(root: &Path) -> Arc<ToolHost> {
    ToolHost::new(
        root.join("config"),
        root.join("tools"),
        root.join("cache"),
        ExtraRoots { dev: Vec::new() },
        "http://127.0.0.1:25150".into(),
        "http://127.0.0.1:25151".into(),
        Arc::new(|| json!({})),
    )
    .await
    .unwrap()
}
async fn fixture(root: &Path) -> (Arc<ToolHost>, catalog::Tool) {
    put(&root.join("tools/old"), "old", "");
    put(
        &root.join("tools/consumer"),
        "consumer",
        "[[requires]]\nid='old'\nversion='^1'\n",
    );
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
    let host = host(root).await;
    fs::create_dir_all(host.tool_data("old")).unwrap();
    fs::write(host.tool_data("old").join("save"), b"unchanged\0data").unwrap();
    fs::create_dir_all(host.cache.join("old")).unwrap();
    fs::write(host.cache.join("old/item"), "cached").unwrap();
    atomic_json(
        &host.tool_settings("old"),
        &json!({"secret":"retain me", "count":42}),
    )
    .unwrap();
    let mut tool = tool();
    tool.source = "fixture/source".into();
    *host.catalog.lock() = catalog::Catalog {
        catalog_version: 1,
        tools: vec![tool.clone()],
        ..catalog::Catalog::default()
    };
    (host, tool)
}

#[tokio::test]
async fn offer_requires_same_trusted_source_and_unique_claim() {
    let root = tempfile::tempdir().unwrap();
    let (host, mut tool) = fixture(root.path()).await;
    assert_eq!(host.update_target("old").as_deref(), Some("new"));
    assert!(
        host.update_available("old", Some("99.0.0")),
        "an identity update is not ordered by version"
    );
    assert_eq!(
        host.view(&host.entry("old").unwrap())["update_target"],
        "new"
    );
    host.saved.lock().get_mut("old").unwrap().trusted = false;
    assert!(host.update_target("old").is_none());
    assert!(
        host.prepare_replacement("old", &tool, true)
            .unwrap_err()
            .contains("trusted catalog source")
    );
    host.saved.lock().get_mut("old").unwrap().trusted = true;
    tool.source = "other/source".into();
    *host.catalog.lock() = catalog::Catalog {
        catalog_version: 1,
        tools: vec![tool.clone()],
        ..catalog::Catalog::default()
    };
    assert!(host.update_target("old").is_none());
    assert!(
        host.replacement_for("old")
            .err()
            .unwrap()
            .contains("different catalog source")
    );
    assert!(
        host.prepare_replacement("old", &tool, true)
            .unwrap_err()
            .contains("trusted catalog source")
    );
    tool.source = "fixture/source".into();
    let mut other = tool.clone();
    other.id = "ambiguous".into();
    host.catalog.lock().tools = vec![tool, other];
    assert!(
        host.replacement_for("old")
            .err()
            .unwrap()
            .contains("ambiguous")
    );
    assert!(!host.replacement_journal().exists());
    assert!(host.data.join("old/tool.toml").exists());
    host.shutdown().await;
}
#[tokio::test]
async fn refuses_every_existing_target_without_changing_state() {
    for slot in 0..5 {
        let root = tempfile::tempdir().unwrap();
        let (host, tool) = fixture(root.path()).await;
        let path = match slot {
            0 => host.data.join("new"),
            1 => host.tool_data("new"),
            2 => host.tool_settings("new"),
            3 => host.cache.join("new"),
            _ => {
                host.saved.lock().insert("new".into(), Saved::default());
                host.data.join("not-a-target")
            }
        };
        if slot < 4 {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if slot == 2 {
                fs::write(&path, "do not overwrite").unwrap();
            } else {
                fs::create_dir(&path).unwrap();
            }
        }
        assert!(
            host.prepare_replacement("old", &tool, true)
                .unwrap_err()
                .contains("already exists")
        );
        assert!(!host.replacement_journal().exists());
        assert!(host.data.join("old/tool.toml").exists());
        assert_eq!(
            fs::read(host.tool_data("old").join("save")).unwrap(),
            b"unchanged\0data"
        );
        if slot == 2 {
            assert_eq!(fs::read_to_string(path).unwrap(), "do not overwrite");
        }
        host.shutdown().await;
    }
}

#[tokio::test]
async fn every_crash_boundary_recovers_data_settings_state_and_dependencies_idempotently() {
    for cut in 0..8 {
        let root = tempfile::tempdir().unwrap();
        let (original, tool) = fixture(root.path()).await;
        put(&original.replacement_stage(), "new", "");
        original.prepare_replacement("old", &tool, true).unwrap();
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
        original.shutdown().await;
        drop(original);
        let recovered = host(root.path()).await;
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
        let consumer = recovered.entry("consumer").unwrap();
        assert_eq!(recovered.view(&consumer)["requires"][0]["satisfied"], true);
        assert!(
            recovered
                .blocked(&consumer)
                .unwrap()
                .contains("unavailable dependency")
        );
        recovered.saved.lock().get_mut("new").unwrap().enabled = Some(true);
        assert!(recovered.blocked(&consumer).is_none());
        assert_eq!(recovered.dependency("old").unwrap().id, "new");
        let mut order = Vec::new();
        recovered
            .visit(
                "consumer",
                &mut HashSet::new(),
                &mut HashSet::new(),
                &mut order,
            )
            .unwrap();
        assert_eq!(order, ["new", "consumer"]);
        assert_eq!(
            recovered.view(&recovered.entry("new").unwrap())["required_by"],
            json!(["consumer"])
        );
        assert!(
            recovered
                .uninstall_catalog("new", false)
                .await
                .unwrap_err()
                .contains("requires")
        );
        recovered.shutdown().await;
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
