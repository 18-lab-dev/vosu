//! J2: real plugin invocation, launch tokens, job ownership and HTTP policy.
use super::support::{Harness, bounded, manifest};
use axum::{
    body::{Body, Bytes, to_bytes},
    extract::Request,
    http::{Method, StatusCode},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use vosu::server::api_v1::{
    actions::ActionRoutes, router::ApiV1Router, subscriptions::ApiV1SubscriptionHub,
};
use vosu_core::{
    actions::JobRegistry,
    identity::PluginInstanceId,
    plugin::{self, PluginHost},
};
use vosu_plugin_api::{
    ActionResult, ActionStatus, CborValue, HostServices, Init, Message, Plugin, PluginError,
    VtableHostServices,
};

use vosu_core::actions::{ActionDescriptor, ActionRegistry};

struct FixedAction(Arc<ActionDescriptor>);
impl ActionRegistry for FixedAction {
    fn get(&self, id: &str) -> Option<Arc<ActionDescriptor>> {
        (id == self.0.id).then(|| self.0.clone())
    }
}
mod fixture {
    use super::*;
    pub struct Echo(VtableHostServices);
    impl Plugin for Echo {
        fn create(host: VtableHostServices, _: Init) -> Result<Self, PluginError> {
            Ok(Self(host))
        }
        fn on_message(&mut self, message: Message, _: u64) -> Result<(), PluginError> {
            match message {
                Message::ActionInvoke(action)
                    if action.args != CborValue::Text("pending".into()) =>
                {
                    assert_eq!(
                        self.0.send(
                            &Message::ActionResult(ActionResult {
                                invocation: action.invocation,
                                status: ActionStatus::Ok,
                                value: Some(action.args),
                                code: None,
                                message: None,
                            }),
                            0
                        ),
                        vosu_plugin_api::vosu_status::VOSU_OK
                    );
                }
                Message::ActionCancel(cancel) => {
                    assert_eq!(
                        self.0.send(
                            &Message::ActionResult(ActionResult {
                                invocation: cancel.invocation,
                                status: ActionStatus::Error,
                                value: None,
                                code: Some("cancelled".into()),
                                message: None,
                            }),
                            0
                        ),
                        vosu_plugin_api::vosu_status::VOSU_OK
                    );
                }
                _ => {}
            }
            Ok(())
        }
    }
    vosu_plugin_internal::export_plugin!(bundled Echo);
}
struct Installed(std::sync::Weak<PluginHost>);
impl vosu::tools::capabilities::InstalledCapabilities for Installed {
    fn version(&self, capability: &str) -> Option<semver::Version> {
        self.0.upgrade()?.installed_capability_version(capability)
    }
    fn validation_snapshot(&self) -> plugin::ValidationSnapshot {
        self.0
            .upgrade()
            .map_or_else(Default::default, |host| host.validation_snapshot())
    }
}
fn load(h: &Harness, root: &std::path::Path) {
    let mut text = manifest("fixture.jtwo");
    for (name, mode, cli) in [
        ("echo", "optional", true),
        ("required", "required", true),
        ("private", "none", false),
    ] {
        text += &format!(
            "\n[[actions]]\nname='{name}'\nversion='1.0.0'\ntitle='Echo'\nargs_schema='args.json'\nresult_schema='result.json'\nsession='{mode}'\ncli={cli}\n[[provides]]\nid='action:fixture.jtwo.{name}'\nversion='1.0.0'\n"
        );
    }
    text += "\n[[provides]]\nid='field:fixture.jtwo'\nversion='1.4.0'\nschema='state.json'\n[[provides]]\nid='field:fixture.jtwo.value'\nversion='1.4.0'\n[[provides]]\nid='game:jtwo'\nversion='*'\n[match]\nprocess_names=[]\n";
    let loaded = plugin::load_bundled(
        &semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap(),
        &h.host.validation_snapshot(),
        &[plugin::BundledPlugin {
            manifest: &text,
            settings_schema: None,
            namespace_schemas: &[r#"{"title":"fixture.jtwo","version":"1.4.0","type":"object","properties":{"value":{"type":"object","properties":{"child":{"type":"number"}}}}}"#],
            action_schemas: &[
                ("args.json", "{\"type\":\"string\"}"),
                ("result.json", "{\"type\":\"string\"}"),
            ],
            vtable: fixture::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    h.host
        .create_instance(PluginInstanceId::new(782301), &loaded[0], root.into(), None)
        .unwrap();
}
#[expect(
    clippy::disallowed_methods,
    reason = "isolated temporary tool fixtures, not user state"
)]
async fn tools(h: &Harness, root: &std::path::Path) -> Arc<vosu::tools::ToolHost> {
    let path = root.join("tool");
    std::fs::create_dir_all(path.join("web")).unwrap();
    std::fs::write(path.join("web/index.html"), "fixture").unwrap();
    std::fs::write(path.join("manifest.toml"), format!("manifest_version=1\nkind='tool'\nid='j-two'\nname='J two'\nversion='1.0.0'\nhost='={}'\n[ui]\nroot='web'\n[[requires]]\nid='action:fixture.jtwo.echo'\nversion='*'\n[[requires]]\nid='field:fixture.jtwo.value.child'\nversion='^1.0.0'\n[[requires]]\nid='game:jtwo'\nversion='*'\n", env!("CARGO_PKG_VERSION"))).unwrap();
    let tools = vosu::tools::ToolHost::new(
        root.join("config"),
        root.join("data"),
        root.join("cache"),
        vosu::tools::ExtraRoots { dev: vec![path] },
        "http://127.0.0.1:25050".into(),
        "http://127.0.0.1:25051".into(),
        Arc::new(|| json!({})),
    )
    .await
    .unwrap();
    tools.set_action_providers(h.host.clone(), Arc::new(Installed(Arc::downgrade(&h.host))));
    tools
}
fn request(method: Method, path: &str, token: Option<&str>, body: Value) -> Request {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:25050");
    if let Some(token) = token {
        request = request.header("X-Vosu-Tool-Session", token);
    }
    request.body(Body::from(body.to_string())).unwrap()
}
async fn value(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &bounded(to_bytes(response.into_body(), 2 * 1024 * 1024))
            .await
            .unwrap(),
    )
    .unwrap()
}
async fn launch(tools: &Arc<vosu::tools::ToolHost>) -> String {
    let response = tools
        .dashboard(
            request(Method::POST, "/api/tools/j-two/launch", None, Value::Null),
            Bytes::new(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = value(response).await;
    let url = url::Url::parse(body["url"].as_str().unwrap()).unwrap();
    let fragment = url.fragment().unwrap();
    let code = url::form_urlencoded::parse(fragment.as_bytes())
        .find(|(key, _)| key == "vosu-launch")
        .unwrap()
        .1
        .into_owned();
    let response = tools
        .overlay(
            request(Method::POST, "/tools/_host/v1/session", None, Value::Null),
            Bytes::from(json!({"tool":"j-two","code":code}).to_string()),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    value(response).await["session"].as_str().unwrap().into()
}

#[test]
fn action_jobs_authority_limits_and_lifecycle() {
    let h = Harness::new();
    h.runtime.block_on(bounded(async {
        let root = tempfile::tempdir().unwrap();
        load(&h, root.path());
        let tools = tools(&h, root.path()).await;
        let first = launch(&tools).await;
        let foreign = launch(&tools).await;
        let source = h.host.api_state_source();
        let hub = ApiV1SubscriptionHub::new(source.clone(), Duration::ZERO);
        let jobs = JobRegistry::new();
        let api = ApiV1Router::new(source, hub.clone()).with_actions(ActionRoutes::new(
            h.host.clone(),
            h.host.invocation_executor(),
            jobs.clone(),
            tools.clone(),
        ));
        let path = "/api/v1/actions/fixture.jtwo.echo";
        let response = api
            .handle(request(Method::POST, path, None, json!({"arguments":"a"})))
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let response = api
            .handle(request(
                Method::POST,
                "/api/v1/actions/fixture.jtwo.private",
                Some(&first),
                json!({"arguments":"a"}),
            ))
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = api
            .handle(request(
                Method::POST,
                "/api/v1/actions/fixture.jtwo.required",
                Some(&first),
                json!({"arguments":"a"}),
            ))
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(value(response).await["error"]["code"], "unknown_action");
        for query in [
            "wait=30001",
            "wait=-1",
            "wait=1&wait=2",
            "after=0",
            "extra=1",
        ] {
            let response = api
                .handle(request(
                    Method::POST,
                    &format!("{path}?{query}"),
                    Some(&first),
                    json!({"arguments":"a"}),
                ))
                .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        }
        let response = api
            .handle(request(
                Method::POST,
                path,
                Some(&first),
                json!({"arguments":42}),
            ))
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(value(response).await["error"]["code"], "invalid_arguments");
        let mut cross = request(Method::POST, path, Some(&first), json!({"arguments":"a"}));
        cross
            .headers_mut()
            .insert("origin", "http://evil.example".parse().unwrap());
        assert_eq!(api.handle(cross).await.status(), StatusCode::FORBIDDEN);
        let response = api
            .handle(request(
                Method::POST,
                &format!("{path}?wait=0"),
                Some(&first),
                json!({"arguments":"pending"}),
            ))
            .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let location = response.headers()["location"].to_str().unwrap().to_owned();
        let job = value(response).await;
        assert_eq!(job["state"], "running");
        assert_eq!(job["session"], Value::Null);
        assert_eq!(
            api.handle(request(Method::GET, &location, Some(&foreign), Value::Null))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            api.handle(request(
                Method::DELETE,
                &location,
                Some(&foreign),
                Value::Null
            ))
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            value(
                api.handle(request(Method::GET, &location, Some(&first), Value::Null))
                    .await
            )
            .await["state"],
            "running"
        );
        let foreign_job = api
            .handle(request(
                Method::POST,
                &format!("{path}?wait=0"),
                Some(&foreign),
                json!({"arguments":"pending"}),
            ))
            .await;
        assert_eq!(foreign_job.status(), StatusCode::ACCEPTED);
        let foreign_location = foreign_job.headers()["location"]
            .to_str()
            .unwrap()
            .to_owned();
        let response = api
            .handle(request(
                Method::GET,
                &format!("{location}?after=0&wait=1"),
                Some(&first),
                Value::Null,
            ))
            .await;
        assert_eq!(value(response).await["revision"], 0);
        let response = api
            .handle(request(
                Method::DELETE,
                &location,
                Some(&first),
                Value::Null,
            ))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let response = api
            .handle(request(
                Method::GET,
                &format!("{location}?after=0&wait=1000"),
                Some(&first),
                Value::Null,
            ))
            .await;
        assert_eq!(value(response).await["state"], "cancelled");
        let response = api
            .handle(request(
                Method::POST,
                path,
                Some(&first),
                json!({"arguments":"distinct-result"}),
            ))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(value(response).await["result"], "distinct-result");
        // Fits JSON's cap, but not the complete ABI envelope.
        let expanded = json!({"arguments":"x".repeat(1024 * 1024 - 50)});
        assert!(expanded.to_string().len() < 1024 * 1024);
        let response = api
            .handle(request(Method::POST, path, Some(&first), expanded))
            .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(value(response).await["error"]["code"], "args_too_large");
        // Rejected preparation must not leave a running job occupying capacity.
        for _ in 1..vosu_core::actions::jobs::MAX_JOBS {
            assert!(
                jobs.create(
                    vosu_core::actions::Principal::Broker,
                    "capacity.fixture".into(),
                    None
                )
                .is_ok()
            );
        }
        let response = api
            .handle(request(
                Method::POST,
                path,
                Some(&first),
                json!({"arguments":"capacity"}),
            ))
            .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(value(response).await["error"]["code"], "too_many_jobs");
        let huge = json!({"arguments":"x".repeat(1024 * 1024)});
        assert_eq!(
            api.handle(request(Method::POST, path, Some(&first), huge.clone()))
                .await
                .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            api.handle(request(Method::GET, &location, Some(&first), huge.clone()))
                .await
                .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            api.handle(request(Method::DELETE, &location, Some(&first), huge))
                .await
                .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        tools.revoke_action_session(&first);
        assert_eq!(
            api.handle(request(
                Method::GET,
                &foreign_location,
                Some(&foreign),
                Value::Null
            ))
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            api.handle(request(Method::GET, &location, Some(&foreign), Value::Null))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            api.handle(request(Method::GET, &location, Some(&first), Value::Null))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert!(
            h.host
                .installed_capability_version("field:fixture.jtwo.value.child")
                .is_some()
        );
        assert!(
            h.host
                .installed_capability_version("field:fixture.jtwoOther.value")
                .is_none()
        );
        // Q7 requires this owner's whole-namespace schema declaration, which
        // covers valueOther too. Preserve the independent leaf-boundary check.
        let leaf = vosu_plugin_api::manifest::Requirement::parse(
            "field:fixture.jtwo.valueOther",
            "^1.0.0",
        )
        .unwrap();
        assert!(
            !leaf.satisfied_by_version("field:fixture.jtwo.value", &semver::Version::new(1, 4, 0),)
        );
        assert!(
            h.host
                .installed_capability_version("field:fixture.jtwo.valueOther")
                .is_some()
        );
        assert!(h.host.installed_capability_version("game:jtwo").is_some());
        // Keep action metadata available: loss below must be caused by the
        // required field/game providers, not merely an absent action.
        let fixed = Arc::new(FixedAction(h.host.get("fixture.jtwo.echo").unwrap()));
        tools.set_action_providers(fixed.clone(), Arc::new(Installed(Arc::downgrade(&h.host))));
        tokio::task::spawn_blocking({
            let host = h.host.clone();
            move || host.destroy_all(Duration::from_secs(5))
        })
        .await
        .unwrap();
        assert!(h.host.installed_capability_version("game:jtwo").is_none());
        assert!(
            h.host
                .installed_capability_version("field:fixture.jtwo.value.child")
                .is_none()
        );
        assert!(fixed.get("fixture.jtwo.echo").is_some());
        assert!(tools.authenticate_action_session(&foreign).is_ok());
        assert!(
            tools
                .authorize_action_session(&foreign, "fixture.jtwo.echo")
                .is_err()
        );
        tools.shutdown().await;
        hub.shutdown();
    }));
}
