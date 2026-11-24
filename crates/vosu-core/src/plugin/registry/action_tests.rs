//! Confined schema assets for two independent actions in one package.
use super::*;

fn manifest() -> String {
    let mut text = "kind='plugin'\nmanifest_version=1\nid='test.assets'\nname='Action assets'\nversion='1.0.0'\nhost='>=1.0.0'\nabi_version=1\nruntime='in-process'\n".to_owned();
    for name in ["one", "two"] {
        text.push_str(&format!("\n[[provides]]\nid='action:test.assets.{name}'\nversion='1.0.0'\n[[actions]]\nname='{name}'\nversion='1.0.0'\ntitle='{name}'\nargs_schema='schemas/{name}.json'\nresult_schema='schemas/shared.json'\nsession='none'\n"));
    }
    text
}

#[test]
fn bundled_assets_use_exact_paths_and_require_every_declared_schema() {
    let text = manifest();
    let assets = [
        ("schemas/one.json", r#"{"const":11}"#),
        ("schemas/two.json", r#"{"const":23}"#),
        ("schemas/shared.json", r#"{"type":"boolean"}"#),
    ];
    let entry = BundledPlugin {
        manifest: &text,
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &assets,
        vtable: vosu_plugin_testkit::echo::vosu_plugin_vtable,
    };
    let loaded = load_bundled(&[entry]).unwrap();
    let schemas = loaded[0].action_schemas();
    assert_eq!(schemas.len(), 3);
    assert_eq!(schemas["schemas/one.json"], assets[0].1);
    assert_eq!(schemas["schemas/two.json"], assets[1].1);
    assert_eq!(schemas["schemas/shared.json"], assets[2].1);
    assert!(
        load_bundled(&[BundledPlugin {
            action_schemas: &assets[..2],
            ..entry
        }])
        .is_err()
    );
    let duplicate = [assets[0], assets[0], assets[1], assets[2]];
    assert!(
        load_bundled(&[BundledPlugin {
            action_schemas: &duplicate,
            ..entry
        }])
        .is_err()
    );
}

#[test]
fn shared_schema_is_loaded_once_and_failures_propagate() {
    let manifest = PluginManifest::parse(&manifest()).unwrap();
    let mut paths = Vec::new();
    let assets = load_action_schemas(&manifest, |path| {
        paths.push(path.to_owned());
        Ok(format!("schema:{path}"))
    })
    .unwrap();
    assert_eq!(
        paths,
        [
            "schemas/one.json",
            "schemas/shared.json",
            "schemas/two.json"
        ]
    );
    assert_eq!(assets["schemas/two.json"], "schema:schemas/two.json");
    assert!(
        load_action_schemas(&manifest, |_| Err(std::io::Error::from(
            std::io::ErrorKind::NotFound
        )
        .into()))
        .is_err()
    );
}

#[test]
fn development_assets_are_required_before_loading_native_code() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("manifest.toml"), manifest()).unwrap();
    std::fs::create_dir(dir.path().join("schemas")).unwrap();
    // Deliberately missing the second schema: a load must fail before dlopen.
    std::fs::write(dir.path().join("schemas/one.json"), "true").unwrap();
    std::fs::write(dir.path().join("schemas/shared.json"), "false").unwrap();
    assert!(matches!(
        load_development(dir.path(), true),
        Err(HostError::Io(_))
    ));
    std::fs::write(dir.path().join("schemas/two.json"), "{}").unwrap();
    // No target was declared, so reaching MissingTarget proves assets loaded.
    assert!(matches!(
        load_development(dir.path(), true),
        Err(HostError::MissingTarget)
    ));
}

#[cfg(unix)]
#[test]
fn development_action_schema_symlink_cannot_escape_package() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("manifest.toml"), manifest()).unwrap();
    std::fs::write(outside.path().join("one.json"), "true").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("schemas")).unwrap();
    match load_development(dir.path(), true) {
        Err(HostError::Io(error)) => assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied),
        _ => panic!("escaped schema was not rejected"),
    }
}
