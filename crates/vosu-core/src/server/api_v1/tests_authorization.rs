use super::super::actions::ActionAuthority;
use super::*;
use axum::{
    body::Body,
    http::{HeaderValue, Method},
};
use vosu_proptest::bounded;

/// The action authority of a tool registry with no launch sessions: no
/// token authenticates, so only keys and the broker remain to be judged.
struct NoSessions;

impl ActionAuthority for NoSessions {
    fn authenticate_action_session(&self, _: &str) -> Option<Principal> {
        None
    }
    fn authorize_action_session(&self, _: &str, _: &str) -> Option<Principal> {
        None
    }
}

impl crate::actions::ToolAuthority for NoSessions {
    fn authorizes(
        &self,
        _: &crate::actions::JobOwner,
        _: &crate::actions::ActionDescriptor,
    ) -> bool {
        false
    }
}

#[tokio::test]
async fn broker_credentials_never_bypass_peer_or_origin_and_keys_are_not_actions() {
    bounded!(async {
        let root = tempfile::tempdir().unwrap();
        let tools: Arc<NoSessions> = Arc::new(NoSessions);
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
        let settings = Arc::new(crate::plugin::settings::ConfigSettingsBridgeB2::new(config));
        let host = crate::plugin::PluginHost::new(
            logger,
            Arc::new(crate::plugin::settings::PluginSettingsService::new(
                settings,
            )),
            tokio::runtime::Handle::current(),
        );
        let routes = super::super::actions::ActionRoutes::new(
            Arc::new(NonCli),
            host.invocation_executor(),
            crate::actions::JobRegistry::new(),
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
    });
}

struct NonCli;
impl crate::actions::ActionRegistry for NonCli {
    fn get(&self, id: &str) -> Option<Arc<crate::actions::ActionDescriptor>> {
        let schema = Arc::new(
            vosu_plugin_api::manifest::action_schema::CompiledActionSchema::compile("true")
                .unwrap(),
        );
        Some(Arc::new(crate::actions::ActionDescriptor {
            id: id.into(),
            version: semver::Version::new(1, 0, 0),
            provider: crate::identity::PluginInstanceId::new(782302),
            provider_id: "fixture.jtwo.broker".into(),
            title: "Not CLI".into(),
            session: vosu_plugin_api::ActionSessionMode::None,
            cli: false,
            args_schema: schema.clone(),
            result_schema: schema,
        }))
    }
}
