use super::*;
use crate::{
    api::{ApiPlan, SessionTarget},
    plugin::{self, PluginInstanceId},
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_route_uses_production_provider_composition() {
    vosu_proptest::bounded!(async {
        let root = tempfile::tempdir().unwrap();
        let config = Arc::new(
            crate::config::ConfigStore::initialize(root.path().join("vosu.toml"))
                .await
                .unwrap(),
        );
        let logger = crate::logging::Logger::new(
            root.path(),
            crate::logging::LoggerOptions::default(),
            Arc::new(crate::logging::StdoutConsole),
        )
        .unwrap();
        let settings = Arc::new(plugin::settings::ConfigSettingsBridgeB2::new(
            config.clone(),
        ));
        let host = PluginHost::new(
            logger.clone(),
            Arc::new(plugin::settings::PluginSettingsService::new(settings)),
            tokio::runtime::Handle::current(),
        );
        let manifest = "kind='plugin'\nmanifest_version=1\nid='test.r4.composition'\nname='R4 composition'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\nroot_slots=['fixture']\n[match]\nprocess_names=['test.r4.composition']\n[[provides]]\nid='field:test.r4.composition'\nversion='1.0.0'\nschema='namespace.json'\n[[provides]]\nid='resource:test.r4.composition.fixture-file'\nversion='1.0.0'\n[[provides]]\nid='resource:test.r4.composition.fixture-stream'\nversion='1.0.0'\n[[provides]]\nid='resource:test.r4.composition.fixture-tree'\nversion='1.0.0'\n[[resource_kinds]]\nkind='fixture-file'\nfamilies=['chart-text']\n[[resource_kinds]]\nkind='fixture-stream'\nfamilies=['chart-text']\n[[resource_kinds]]\nkind='fixture-tree'\ntree=true\nfamilies=['chart-text']\n[[provides]]\nid='game:fixture'\nversion='*'\n";
        let schema =
            r#"{"title":"test.r4.composition","version":"1.0.0","type":"object","properties":{}}"#;
        let loaded = plugin::load_bundled(
            &semver::Version::parse("2.0.0-alpha.1").unwrap(),
            &crate::plugin::ValidationSnapshot::default(),
            &[plugin::BundledPlugin {
                manifest,
                settings_schema: None,
                namespace_schemas: &[schema],
                action_schemas: &[],
                vtable: vosu_plugin_internal::vtable_for::<
                    vosu_plugin_testkit::fixture::ApiFixturePlugin,
                >,
            }],
        )
        .unwrap();
        host.create_instance(
            PluginInstanceId::new(9471),
            &loaded[0],
            root.path().into(),
            None,
        )
        .unwrap();
        host.process_appeared(plugin::ProcessRecord {
            pid: 947193,
            name: "test.r4.composition".into(),
            path: "/private/r4-provider-sentinel".into(),
            command_line: String::new(),
            argv: None,
            bitness: vosu_plugin_api::Bitness::X64,
            start_time: 19,
        });
        let plan = ApiPlan::new(&["/test.r4.composition/resources/stream/url"]).unwrap();
        let url = loop {
            if let Ok(update) = host.api_state_source().read(&SessionTarget::Active, &plan)
                && let Some(Some(crate::state::StateValue::String(url))) = update.values.first()
            {
                break url.to_string();
            }
            tokio::task::yield_now().await;
        };
        let service = ApiV1Service::new(&host, &Config::default());
        let response = service
            .router
            .handle(Request::builder().uri(url).body(Body::empty()).unwrap())
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), vosu_plugin_testkit::fixture::STREAM_BYTES);
        service.shutdown();
        host.destroy_all(Duration::from_secs(2));
        config.shutdown().await.unwrap();
        logger.shutdown().await.unwrap();
    });
}

#[tokio::test]
async fn production_resource_startup_requires_a_persistent_valid_secret() {
    vosu_proptest::bounded!(async {
        let root = tempfile::tempdir().unwrap();
        let store = resource_store(root.path().to_path_buf()).await.unwrap();
        assert!(store.is_persistent());
        std::fs::write(
            root.path().join("resource-secret"),
            b"corrupt-secret-sentinel",
        )
        .unwrap();
        assert!(resource_store(root.path().to_path_buf()).await.is_err());
        let not_directory = root.path().join("not-a-directory");
        std::fs::write(&not_directory, b"cannot-create-resource-storage").unwrap();
        assert!(resource_store(not_directory).await.is_err());
    });
}
