use super::*;
use crate::tools::ToolHost;
use axum::{
    body::Body,
    http::{HeaderValue, Method},
};
use serde_json::json;
use vosu_proptest::bounded;

#[tokio::test]
async fn broker_credentials_never_bypass_peer_or_origin_and_keys_are_not_actions() {
    bounded!(async {
        let root = tempfile::tempdir().unwrap();
        let tools = ToolHost::new(
            root.path().join("config"),
            root.path().join("data"),
            root.path().join("cache"),
            crate::tools::ExtraRoots { dev: vec![] },
            "http://127.0.0.1:25050".into(),
            "http://127.0.0.1:25051".into(),
            Arc::new(|| json!({})),
        )
        .await
        .unwrap();
        let path = root.path().join("broker.json");
        let broker = Arc::new(BrokerToken::new(path.clone()).unwrap());
        broker.publish("127.0.0.1:25050".parse().unwrap()).unwrap();
        let descriptor: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let mut request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/actions/fixture.echo")
            .header(header::HOST, "127.0.0.1:25050")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", descriptor["token"].as_str().unwrap()),
            )
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(Some(broker.clone()));
        request
            .extensions_mut()
            .insert(ConnectInfo(PeerAddress("127.0.0.1:43321".parse().unwrap())));
        assert!(matches!(
            Credentials::authenticate(&request, tools.as_ref())
                .unwrap()
                .principal,
            Principal::Broker
        ));
        let config = Arc::new(
            vosu_core::config::ConfigStore::initialize(root.path().join("vosu.toml"))
                .await
                .unwrap(),
        );
        let logger = vosu_core::logging::Logger::new(
            root.path(),
            vosu_core::logging::LoggerOptions::default(),
            Arc::new(vosu_core::logging::StdoutConsole),
        )
        .unwrap();
        let settings = Arc::new(vosu_core::plugin::settings::ConfigSettingsBridgeB2::new(
            config,
        ));
        let host = vosu_core::plugin::PluginHost::new(
            logger,
            Arc::new(vosu_core::plugin::settings::PluginSettingsService::new(
                settings,
            )),
            tokio::runtime::Handle::current(),
        );
        let routes = super::super::actions::ActionRoutes::new(
            Arc::new(NonCli),
            host.invocation_executor(),
            vosu_core::actions::JobRegistry::new(),
            tools.clone(),
        );
        let mut invoke = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/actions/fixture.echo")
            .body(Body::empty())
            .unwrap();
        *invoke.headers_mut() = request.headers().clone();
        invoke.extensions_mut().insert(Some(broker.clone()));
        invoke
            .extensions_mut()
            .insert(ConnectInfo(PeerAddress("127.0.0.1:43321".parse().unwrap())));
        let response = routes
            .create(&host.api_state_source(), invoke, None, "fixture.echo")
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        request.extensions_mut().insert(ConnectInfo(PeerAddress(
            "192.0.2.55:43321".parse().unwrap(),
        )));
        assert_eq!(
            Credentials::authenticate(&request, tools.as_ref())
                .err()
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        request
            .extensions_mut()
            .insert(ConnectInfo(PeerAddress("127.0.0.1:43321".parse().unwrap())));
        request.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.example"),
        );
        assert_eq!(
            Credentials::authenticate(&request, tools.as_ref())
                .err()
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        request.headers_mut().remove(header::ORIGIN);
        request
            .headers_mut()
            .insert(TOOL_SESSION, HeaderValue::from_static("other-credential"));
        assert_eq!(
            Credentials::authenticate(&request, tools.as_ref())
                .err()
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        request.headers_mut().remove(TOOL_SESSION);
        broker.disable();
        assert_eq!(
            Credentials::authenticate(&request, tools.as_ref())
                .err()
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer vosu_key_fixture"),
        );
        assert_eq!(
            Credentials::authenticate(&request, tools.as_ref())
                .err()
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        tools.shutdown().await;
    });
}

struct NonCli;
impl vosu_core::actions::ActionRegistry for NonCli {
    fn get(&self, id: &str) -> Option<Arc<vosu_core::actions::ActionDescriptor>> {
        let schema = Arc::new(
            vosu_plugin_api::manifest::action_schema::CompiledActionSchema::compile("true")
                .unwrap(),
        );
        Some(Arc::new(vosu_core::actions::ActionDescriptor {
            id: id.into(),
            version: semver::Version::new(1, 0, 0),
            provider: vosu_core::identity::PluginInstanceId::new(782302),
            provider_id: "fixture.jtwo.broker".into(),
            title: "Not CLI".into(),
            session: vosu_plugin_api::ActionSessionMode::None,
            cli: false,
            args_schema: schema.clone(),
            result_schema: schema,
        }))
    }
}
