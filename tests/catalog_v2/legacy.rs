use super::support::*;
use serde_json::json;
#[test]
fn legacy_discovery_matrix_preserves_files_and_manifest_precedence() {
    let root = tempfile::tempdir().unwrap();
    let static_root = root.path().join("static");
    std::fs::create_dir_all(&static_root).unwrap();
    for name in [
        "metadata",
        "index",
        "manifest",
        "both",
        "invalid",
        "unsupported",
        "unreadable",
    ] {
        let directory = static_root.join(name);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("settings.json"), "[]\r\n").unwrap();
        if name != "manifest" {
            std::fs::write(directory.join("index.html"), "overlay").unwrap();
        }
        if name != "index" && name != "manifest" {
            std::fs::write(directory.join("metadata.txt"),"## comment\r\nName: First\r\nNAME: Legacy\r\nAuthor: Author\r\nAuthorLinks: https://example.org\r\nResolution: broken\r\n").unwrap();
        }
        if ["manifest", "both", "invalid", "unsupported"].contains(&name) {
            let mut value = manifest("overlay");
            value["name"] = json!("Manifest");
            if name == "unsupported" {
                value["host"] = json!("<0.0.1");
            }
            let text = if name == "invalid" {
                "invalid TOML".into()
            } else {
                text(&value)
            };
            std::fs::write(directory.join("manifest.toml"), text).unwrap();
        }
        if name == "unreadable" {
            std::fs::create_dir(directory.join("manifest.toml")).unwrap();
        }
    }
    let service = vosu_overlay_host::CounterService::production(
        static_root.clone(),
        root.path().join("settings"),
        root.path().join("cache"),
    );
    let (found, broken) = service.discover_all().unwrap();
    assert_eq!(found.len(), 4);
    assert_eq!(broken.len(), 3);
    for name in ["manifest", "both"] {
        let row = found.iter().find(|r| r.folder_name == name).unwrap();
        assert_eq!(row.name, "Manifest");
        assert_eq!(row.version, "9.0.0");
    }
    let metadata = found.iter().find(|r| r.folder_name == "metadata").unwrap();
    assert_eq!(metadata.name, "Legacy");
    assert_eq!(metadata.version, "1.0");
    assert_eq!(metadata.resolution, vec![json!("Any"), json!("Any")]);
    assert_eq!(metadata.authorlinks, ["https://example.org"]);
    assert!(broken.iter().all(|r| r.error.starts_with("manifest.toml:")));
    for name in [
        "metadata",
        "index",
        "manifest",
        "both",
        "invalid",
        "unsupported",
        "unreadable",
    ] {
        assert_eq!(
            std::fs::read(static_root.join(name).join("settings.json")).unwrap(),
            b"[]\r\n"
        );
    }
}
#[test]
fn legacy_exact_read_bound_and_utf8_are_enforced_by_real_reader() {
    let mut at_limit = b"Name: Boundary\n".to_vec();
    at_limit.resize(65536, b' ');
    let mut over_limit = at_limit.clone();
    over_limit.push(b' ');
    for (bytes, valid) in [(at_limit, true), (over_limit, false), (vec![0xff], false)] {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("static/legacy");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("metadata.txt"), &bytes).unwrap();
        let service = vosu_overlay_host::CounterService::production(
            root.path().join("static"),
            root.path().join("settings"),
            root.path().join("cache"),
        );
        let (found, broken) = service.discover_all().unwrap();
        assert_eq!(found.len(), usize::from(valid));
        assert_eq!(broken.len(), usize::from(!valid));
        assert_eq!(
            std::fs::read(directory.join("metadata.txt")).unwrap(),
            bytes
        );
    }
}
