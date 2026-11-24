use super::*;

fn manifest(host: &str) -> String {
    let host = if host == "*" {
        format!("={}", crate::application_version())
    } else {
        host.to_owned()
    };
    format!(
        "manifest_version=1\nkind='overlay'\nid='manifest-only'\nname='Manifest overlay'\nversion='7.2.3'\nhost='{host}'\nresolution=[1920,1080]\n"
    )
}

#[test]
fn manifest_only_and_both_files_are_discovered_once_without_writes() {
    let (root, service, _, _) = fixture();
    for folder in ["manifest-only", "both"] {
        let directory = root.path().join("static").join(folder);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("manifest.toml"), manifest("*")).unwrap();
        std::fs::write(directory.join("settings.json"), "[]\n").unwrap();
    }
    let both = root.path().join("static/both");
    let legacy = b"Name: Ignored legacy\r\nVersion: 1.0\r\n";
    std::fs::write(both.join("metadata.txt"), legacy).unwrap();
    std::fs::write(both.join("index.html"), "overlay").unwrap();
    let (found, broken) = service.discover_all().unwrap();
    assert!(broken.is_empty());
    assert_eq!(found.len(), 2);
    for counter in found {
        assert_eq!(counter.name, "Manifest overlay");
        assert_eq!(counter.version, "7.2.3");
        assert_eq!(counter.resolution, vec![json!(1920), json!(1080)]);
        assert_eq!(
            std::fs::read(
                root.path()
                    .join("static")
                    .join(counter.folder_name)
                    .join("settings.json")
            )
            .unwrap(),
            b"[]\n"
        );
    }
    assert_eq!(std::fs::read(both.join("metadata.txt")).unwrap(), legacy);
}

#[test]
fn malformed_unsupported_and_unreadable_manifests_never_fall_back() {
    let (root, service, _, _) = fixture();
    for (folder, text) in [
        ("malformed", "broken TOML".into()),
        ("unsupported", manifest("<1.0.0")),
        (
            "wrong-kind",
            manifest("*").replace("kind='overlay'", "kind='tool'"),
        ),
    ] {
        let directory = root.path().join("static").join(folder);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("manifest.toml"), text).unwrap();
        std::fs::write(directory.join("metadata.txt"), "Name: Valid legacy").unwrap();
        std::fs::write(directory.join("index.html"), "overlay").unwrap();
    }
    let unreadable = root.path().join("static/unreadable");
    std::fs::create_dir_all(unreadable.join("manifest.toml")).unwrap();
    std::fs::write(unreadable.join("metadata.txt"), "Name: Valid legacy").unwrap();
    std::fs::write(unreadable.join("index.html"), "overlay").unwrap();
    let (found, broken) = service.discover_all().unwrap();
    assert!(found.is_empty());
    assert_eq!(broken.len(), 4);
    assert!(
        broken
            .iter()
            .all(|overlay| overlay.error.starts_with("manifest.toml:"))
    );
}

#[cfg(unix)]
#[test]
fn dangling_manifest_symlink_dominates_valid_legacy_metadata() {
    let (root, service, _, _) = fixture();
    let directory = root.path().join("static/symlink");
    std::fs::create_dir(&directory).unwrap();
    std::os::unix::fs::symlink("missing", directory.join("manifest.toml")).unwrap();
    std::fs::write(directory.join("metadata.txt"), "Name: Legacy").unwrap();
    let (found, broken) = service.discover_all().unwrap();
    assert!(found.is_empty());
    assert_eq!(broken.len(), 1);
    assert!(broken[0].error.starts_with("manifest.toml:"));
}
