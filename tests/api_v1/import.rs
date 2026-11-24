//! I1: production registry/schema -> J2 HTTP -> J1 executor -> real osu plugin.
use super::support::{Harness, bounded, record};
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{Method, StatusCode},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use vosu::server::api_v1::{
    actions::{ActionAuthority, ActionRoutes},
    router::ApiV1Router,
    subscriptions::ApiV1SubscriptionHub,
};
use vosu_core::{
    actions::{ActionDescriptor, ActionRegistry, JobOwner, JobRegistry, Principal, ToolAuthority},
    identity::PluginInstanceId,
    plugin,
};
struct Authority(JobOwner);
impl ToolAuthority for Authority {
    fn authorizes(&self, owner: &JobOwner, action: &ActionDescriptor) -> bool {
        owner.same(&self.0) && action.id == vosu_osu::actions::IMPORT_ACTION
    }
}
impl ActionAuthority for Authority {
    fn authenticate_action_session(&self, token: &str) -> Option<Principal> {
        (token == "i1-owned-launch").then(|| Principal::Tool(self.0.clone()))
    }
    fn authorize_action_session(&self, token: &str, action: &str) -> Option<Principal> {
        (action == vosu_osu::actions::IMPORT_ACTION)
            .then(|| self.authenticate_action_session(token))
            .flatten()
    }
}
fn request(method: Method, path: &str, arguments: Value) -> Request {
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:25050")
        .header("x-vosu-tool-session", "i1-owned-launch")
        .body(Body::from(json!({"arguments":arguments}).to_string()))
        .unwrap()
}
async fn body(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &bounded(to_bytes(response.into_body(), 1024 * 1024))
            .await
            .unwrap(),
    )
    .unwrap()
}
#[test]
fn real_osu_import_action_http_registry_schema_and_terminal() {
    let h = Harness::new();
    let loaded = plugin::load_bundled(&vosu::plugins::BUNDLED[..1]).unwrap();
    h.host
        .create_instance(
            PluginInstanceId::new(917310),
            &loaded[0],
            h.root.path().into(),
            Some(plugin::WorkerPool::new(2, 64)),
        )
        .unwrap();
    let descriptor = h.host.get(vosu_osu::actions::IMPORT_ACTION).unwrap();
    assert!(descriptor.cli);
    assert_eq!(
        descriptor.session,
        vosu_plugin_api::ActionSessionMode::Required
    );
    assert!(h.host.get("vosu.osu.beatmap.export").is_none());
    assert_eq!(descriptor.version, semver::Version::new(1, 0, 0));
    let mut process = record(917311, "osu!.exe");
    process.path = h
        .root
        .path()
        .join("osu!.exe")
        .to_string_lossy()
        .into_owned();
    process.argv = Some(vec![process.path.clone()]);
    process.command_line = process.path.clone();
    process.bitness = vosu_plugin_api::Bitness::X86;
    h.host.process_appeared(process);
    h.runtime.block_on(bounded(async {
        while vosu_osu::test_support::session_settings(917311).is_none() {
            tokio::task::yield_now().await;
        }
        let source = h.host.api_state_source();
        let hub = ApiV1SubscriptionHub::new(source.clone(), Duration::ZERO);
        let api = ApiV1Router::new(source, hub.clone()).with_actions(ActionRoutes::new(
            h.host.clone(),
            h.host.invocation_executor(),
            JobRegistry::new(),
            Arc::new(Authority(JobOwner::for_tool("i1-tool".into()))),
        ));
        let action = vosu_osu::actions::IMPORT_ACTION;
        let absent = h.root.path().join("private-missing-source-sentinel.osz");
        let args = json!({"sourcePath":absent});
        assert_eq!(
            api.handle(request(
                Method::POST,
                &format!("/api/v1/actions/{action}"),
                args.clone()
            ))
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        let path = format!("/api/v1/sessions/active/actions/{action}?wait=5000");
        let invalid = api
            .handle(request(Method::POST, &path, json!({"sourcePath":13})))
            .await;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body(invalid).await["error"]["code"], "invalid_arguments");
        let response = api.handle(request(Method::POST, &path, args)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let result = body(response).await;
        assert_eq!(result["state"], "failed");
        assert_eq!(result["error"]["code"], "invalid_archive");
        assert!(
            !result
                .to_string()
                .contains("private-missing-source-sentinel")
        );
        hub.shutdown();
    }));
}
