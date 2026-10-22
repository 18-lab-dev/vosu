use super::*;

fn tool(id: &str, version: &str) -> Tool {
    Tool {
        id: id.into(),
        source: OFFICIAL_SOURCE.into(),
        name: id.into(),
        version: version.into(),
        description: "fixture".into(),
        author: "18-LAB".into(),
        homepage: None,
        protocol: 1,
        interfaces: vec!["ui".into()],
        requires: Vec::new(),
        packages: BTreeMap::from([(
            "any".into(),
            Package {
                url: "https://example.com/tool.zip".into(),
                sha256: "00".repeat(32),
                size: 1,
            },
        )]),
    }
}
fn catalog(tools: Vec<Tool>) -> Catalog {
    Catalog {
        catalog_version: 1,
        published: "2026-10-21T00:00:00Z".into(),
        tools,
        ..Catalog::default()
    }
}
fn zip(entries: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut writer = zip::ZipWriter::new(file.reopen().unwrap());
    for (name, body) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(body).unwrap();
    }
    writer.finish().unwrap();
    file
}
#[test]
fn parsing_and_validation() {
    let valid = catalog(vec![tool("fixture", "1.0.0")]);
    Catalog::validate(&serde_json::from_value(serde_json::to_value(&valid).unwrap()).unwrap())
        .unwrap();
    let mut invalid = valid.clone();
    invalid.catalog_version = 2;
    assert!(invalid.validate().is_err());
    let mut invalid = valid.clone();
    invalid.tools.push(invalid.tools[0].clone());
    assert!(invalid.validate().is_err());
    for id in ["../bad", "", "Upper"] {
        let mut invalid = valid.clone();
        invalid.tools[0].id = id.into();
        assert!(invalid.validate().is_err());
    }
    for url in [
        "http://example.com/a",
        "file:///a",
        "https://user:pass@example.com/a",
    ] {
        let mut invalid = valid.clone();
        invalid.tools[0].packages.get_mut("any").unwrap().url = url.into();
        assert!(invalid.validate().is_err());
    }
    let mut invalid = valid.clone();
    invalid.tools[0].packages.get_mut("any").unwrap().sha256 = "bad".into();
    assert!(invalid.validate().is_err());
    let mut invalid = valid;
    invalid.tools[0].version = "not-semver".into();
    assert!(invalid.validate().is_err());
}
#[test]
fn requires_order_and_cycles() {
    let mut consumer = tool("consumer", "1.0.0");
    consumer.requires = vec![Requirement {
        id: "dependency".into(),
        version: ">=1.0.0".into(),
    }];
    let mut valid = catalog(vec![consumer, tool("dependency", "1.0.0")]);
    valid.validate().unwrap();
    assert_eq!(
        valid
            .order("consumer")
            .unwrap()
            .iter()
            .map(|t| t.id.as_str())
            .collect::<Vec<_>>(),
        ["dependency", "consumer"]
    );
    valid.tools[1].requires = vec![Requirement {
        id: "consumer".into(),
        version: "*".into(),
    }];
    assert!(valid.order("consumer").is_err());
    valid.tools.pop();
    assert!(valid.order("consumer").is_err());
}
#[test]
fn package_integrity_and_atomic_rollback() {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("fixture");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("old"), "previous installation").unwrap();
    let bytes =
        b"manifest_version=1\nid='fixture'\nname='Fixture'\nversion='1.0.0'\n[ui]\nroot='web'\n";
    let archive = zip(&[("tool.toml", bytes), ("web/index.html", b"fixture")]);
    let mut package = Package {
        url: "https://example.com/a".into(),
        size: archive.as_file().metadata().unwrap().len(),
        sha256: sha2::Sha256::digest(fs::read(archive.path()).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    };
    verify_package(archive.path(), &package).unwrap();
    package.size += 1;
    assert!(
        verify_package(archive.path(), &package)
            .unwrap_err()
            .contains("size")
    );
    package.size -= 1;
    package.sha256 = "00".repeat(32);
    assert!(
        verify_package(archive.path(), &package)
            .unwrap_err()
            .contains("SHA-256")
    );
    let cancel = CancellationToken::new();
    assert!(
        extract(
            archive.path(),
            &destination,
            &tool("fixture", "2.0.0"),
            &cancel
        )
        .is_err()
    );
    assert!(destination.join("old").exists());
    for name in ["../escape", "/absolute", "nested/../../escape"] {
        let hostile = zip(&[(name, b"bad")]);
        assert!(
            extract(
                hostile.path(),
                &destination,
                &tool("fixture", "1.0.0"),
                &cancel
            )
            .is_err()
        );
        assert!(destination.join("old").exists());
    }
    extract(
        archive.path(),
        &destination,
        &tool("fixture", "1.0.0"),
        &cancel,
    )
    .unwrap();
    assert!(!destination.join("old").exists());
    assert!(destination.join("web/index.html").exists());
}
async fn host(root: &Path) -> Arc<ToolHost> {
    ToolHost::new(
        root.join("config"),
        root.join("data"),
        root.join("cache"),
        ExtraRoots { dev: Vec::new() },
        "http://127.0.0.1:25150".into(),
        "http://127.0.0.1:25151".into(),
        Arc::new(|| json!({})),
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn source_update_badge_and_uninstall_choices() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path()).await;
    let path = host.data.join("fixture");
    fs::create_dir_all(path.join("web")).unwrap();
    fs::write(
        path.join("tool.toml"),
        "manifest_version=1\nid='fixture'\nname='Fixture'\nversion='1.0.0'\n[ui]\nroot='web'\n",
    )
    .unwrap();
    host.rescan().await;
    assert_eq!(host.view(&host.entry("fixture").unwrap())["source"], "user");
    host.saved
        .lock()
        .entry("fixture".into())
        .or_default()
        .official = true;
    // A source transition also changes the discovery identity.
    host.entries.lock().clear();
    host.rescan().await;
    *host.catalog.lock() = catalog(vec![tool("fixture", "2.0.0")]);
    let entry = host.entry("fixture").unwrap();
    let view = host.view(&entry);
    assert_eq!(view["source"], "official");
    assert_eq!(view["trusted"], true);
    assert_eq!(view["update_available"], true);
    fs::create_dir_all(&entry.data_dir).unwrap();
    fs::write(entry.data_dir.join("save"), "data").unwrap();
    assert!(host.begin_catalog("fixture", "uninstall", b"{}").is_err());
    host.uninstall_catalog("fixture", false).await.unwrap();
    assert!(entry.data_dir.join("save").exists());
    assert!(!path.exists());
    fs::create_dir_all(path.join("web")).unwrap();
    fs::write(
        path.join("tool.toml"),
        "manifest_version=1\nid='fixture'\nname='Fixture'\nversion='1.0.0'\n[ui]\nroot='web'\n",
    )
    .unwrap();
    host.saved
        .lock()
        .entry("fixture".into())
        .or_default()
        .official = true;
    host.rescan().await;
    host.uninstall_catalog("fixture", true).await.unwrap();
    assert!(!entry.data_dir.exists());
    host.shutdown().await;
}

#[test]
fn only_the_default_source_grants_automatic_trust() {
    let mut state = Saved::default();
    record_source(&mut state, OFFICIAL_SOURCE);
    assert!(state.official && state.trusted);
    for source in [
        "another/tools",
        "shiruviaa/vosu-tools@v1.0.0",
        "https://github.com/shiruviaa/vosu-tools/releases/latest/download/catalog.json",
    ] {
        record_source(&mut state, source);
        assert!(!state.official && !state.trusted, "{source}");
        state.trusted = true;
        record_source(&mut state, source);
        assert!(
            state.trusted,
            "an approved same-source update preserves approval"
        );
    }
}

#[tokio::test]
async fn catalog_transport_rejects_http_and_private_destinations() {
    for (url, scheme) in [
        ("http://example.com/catalog.json", true),
        ("https://127.0.0.1/catalog.json", false),
        ("https://10.0.0.1/catalog.json", false),
    ] {
        let error = client(1024)
            .request_bytes(url, "catalog-test", &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(error, crate::net::NetworkError::Scheme) && scheme
                || matches!(error, crate::net::NetworkError::Destination(_)) && !scheme,
            "{error}"
        );
    }
}

#[tokio::test]
async fn durable_data_cannot_collide_with_any_package_id() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path()).await;
    let package = host.data.join("data");
    fs::create_dir_all(package.join("web")).unwrap();
    fs::write(
        package.join("tool.toml"),
        "manifest_version=1\nid='data'\nname='Data'\nversion='1.0.0'\n[ui]\nroot='web'\n",
    )
    .unwrap();
    host.rescan().await;
    let entry = host.entry("data").unwrap();
    assert_eq!(entry.root, package);
    assert!(!entry.data_dir.starts_with(&host.data));
    fs::create_dir_all(&entry.data_dir).unwrap();
    fs::write(entry.data_dir.join("save"), "durable").unwrap();
    fs::remove_dir_all(&entry.root).unwrap();
    assert_eq!(
        fs::read_to_string(entry.data_dir.join("save")).unwrap(),
        "durable"
    );
    host.shutdown().await;
}
