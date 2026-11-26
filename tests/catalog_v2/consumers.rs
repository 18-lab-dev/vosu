use super::support::*;
use serde_json::Value;
use std::time::Duration;

#[test]
fn overlay_corpus_uses_real_manifest_first_discovery() {
    for row in super::manifest::corpus()
        .into_iter()
        .filter(|r| r.valid["kind"] == "overlay")
    {
        for (value, valid) in [(&row.valid, true), (&row.invalid, false)] {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("static/corpus");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("manifest.toml"), text(value)).unwrap();
            let metadata = b"Name: Legacy must not win\r\nVersion: 1.0\r\n";
            std::fs::write(dir.join("metadata.txt"), metadata).unwrap();
            std::fs::write(dir.join("index.html"), "overlay").unwrap();
            std::fs::write(dir.join("settings.json"), "[]\n").unwrap();
            let service = vosu_overlay_host::CounterService::production(
                root.path().join("static"),
                root.path().join("settings"),
                root.path().join("cache"),
            );
            let (found, broken) = service.discover_all().unwrap();
            assert_eq!(found.len(), usize::from(valid), "{}", row.label);
            assert_eq!(broken.len(), usize::from(!valid), "{}", row.label);
            if !valid {
                assert!(
                    broken[0].error.contains(row.rule),
                    "{}: {}",
                    row.label,
                    broken[0].error
                );
            }
            assert_eq!(std::fs::read(dir.join("metadata.txt")).unwrap(), metadata);
            assert_eq!(std::fs::read(dir.join("settings.json")).unwrap(), b"[]\n");
        }
    }
}

#[tokio::test]
async fn tool_corpus_uses_real_discovery_and_dashboard() {
    tokio::time::timeout(Duration::from_secs(90),async {
        for row in super::manifest::corpus().into_iter().filter(|r| r.valid["kind"] == "tool") {
            for (value,valid) in [(&row.valid,true),(&row.invalid,false)] {
                let root = tempfile::tempdir().unwrap(); let dir = root.path().join("data/corpus");
                std::fs::create_dir_all(dir.join("web")).unwrap();
                std::fs::write(dir.join("web/index.html"),"tool").unwrap();
                std::fs::write(dir.join("manifest.toml"),text(value)).unwrap();
                // A valid legacy manifest must never rescue rejected v2 input.
                std::fs::write(dir.join("tool.toml"),"manifest_version=1\nid='corpus'\nname='Legacy'\nversion='1.0.0'\n[ui]\nroot='web'\n").unwrap();
                let host = vosu::tools::ToolRegistry::new(root.path().join("config"),root.path().join("data"),root.path().join("cache"),
                    vosu::tools::ExtraRoots{dev:vec![]},"http://127.0.0.1:1".into(),Some("http://127.0.0.1:2".into())).await.unwrap();
                let response = host.dashboard(axum::http::Request::builder().uri("/api/tools").body(axum::body::Body::empty()).unwrap(),bytes::Bytes::new()).await;
                let body = axum::body::to_bytes(response.into_body(),1024*1024).await.unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                let tools = body["tools"].as_array().unwrap(); assert_eq!(tools.len(),1,"{}",row.label);
                assert_eq!(tools[0]["status"] == "invalid",!valid,"{}: {}",row.label,tools[0]);
                host.shutdown().await;
            }
        }
    }).await.expect("bounded tool corpus");
}

#[cfg(feature = "test-harness")]
#[test]
fn plugin_corpus_uses_real_core_bundled_loader() {
    for row in super::manifest::corpus()
        .into_iter()
        .chain(super::plugin::corpus())
        .chain(super::declarations::corpus())
        .filter(|r| r.valid["kind"] == "plugin")
    {
        for (value, valid) in [(&row.valid, true), (&row.invalid, false)] {
            let text = text(value);
            let result = vosu_core::plugin::load_bundled(
                vosu::application_version(),
                &vosu_core::plugin::ValidationSnapshot::default(),
                &[vosu_core::plugin::BundledPlugin {
                    manifest: &text,
                    settings_schema: None,
                    namespace_schemas: &[],
                    action_schemas: &[("args.json", "true"), ("result.json", "true")],
                    vtable: vosu_plugin_testkit::echo::vosu_plugin_vtable,
                }],
            );
            assert_eq!(result.is_ok(), valid, "{}: {:?}", row.label, result.err());
        }
    }
}
