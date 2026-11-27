use super::Hosts;
use reqwest::{Method, RequestBuilder, StatusCode, header};
use serde_json::{Value, json};
use std::{
    hint::black_box,
    path::{Path, PathBuf},
};
use vosu_core::plugin::BundledPlugin;
use vosu_plugin_testkit::fixture::ApiFixturePlugin as ActionProvider;

const ID: &str = "m7-d17";
const ACTION: &str = "test.api.fixture.run";
const START: &str = "/api/v1/actions/test.api.fixture.run?wait=0";
const TOOL_MANIFEST: &str = r#"
kind = 'tool'
manifest_version = 1
id = 'm7-d17'
name = 'M7 D17 action client'
version = '1.0.0'
host = '>=2.0.0-alpha.1, <3.0.0'
[ui]
root = 'web'
[[requires]]
id = 'action:test.api.fixture.run'
version = '*'
"#;

vosu_plugin_internal::export_plugin!(bundled ActionProvider);

pub fn provider() -> BundledPlugin<'static> {
    BundledPlugin {
        manifest: include_str!("../../../tests/fixtures/api-v1-plugin/manifest.toml"),
        settings_schema: None,
        namespace_schemas: &[include_str!(
            "../../../tests/fixtures/api-v1-plugin/namespace.json"
        )],
        action_schemas: &[
            (
                "args.json",
                include_str!("../../../tests/fixtures/api-v1-plugin/args.json"),
            ),
            (
                "result.json",
                include_str!("../../../tests/fixtures/api-v1-plugin/result.json"),
            ),
        ],
        vtable: vosu_plugin_vtable,
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "test package in an isolated temp directory"
)]
pub fn package(root: &Path) -> PathBuf {
    let package = root.join(ID);
    std::fs::create_dir_all(package.join("web")).unwrap();
    std::fs::write(package.join("manifest.toml"), TOOL_MANIFEST).unwrap();
    std::fs::write(
        package.join("web/index.html"),
        "<!doctype html><title>M7 D17</title>",
    )
    .unwrap();
    package
}

pub struct Client {
    http: reqwest::Client,
    overlay: String,
    token: String,
    pub jobs: u64,
}

async fn json_response(request: RequestBuilder, status: StatusCode) -> Value {
    let response = request.send().await.unwrap();
    let code = response.status();
    let body = response.bytes().await.unwrap();
    assert_eq!(code, status, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}

impl Client {
    pub async fn launch(hosts: &Hosts) -> Self {
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let dashboard = http
            .get(format!("{}/tools", hosts.dashboard))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap();
        let page_context = dashboard
            .split_once("id=\"vosu-context\" type=\"application/json\">")
            .unwrap()
            .1
            .split_once("</script>")
            .unwrap()
            .0;
        let context: Value = serde_json::from_str(page_context).unwrap();
        let csrf = context["csrf"].as_str().unwrap();
        let launch = json_response(
            http.post(format!("{}/api/tools/{ID}/launch", hosts.dashboard))
                .header(header::ORIGIN, &hosts.dashboard)
                .header("x-vosu-csrf", csrf)
                .json(&json!({"theme": "dark"})),
            StatusCode::OK,
        )
        .await;
        let url = url::Url::parse(launch["url"].as_str().unwrap()).unwrap();
        assert_eq!(url.origin().ascii_serialization(), hosts.overlay);
        let page = http
            .get(url.clone())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(page.contains("<title>M7 D17</title>"));
        let code = url::form_urlencoded::parse(url.fragment().unwrap().as_bytes())
            .find(|(key, _)| key == "vosu-launch")
            .unwrap()
            .1;
        let exchange = json_response(
            http.post(format!("{}/tools/_host/v1/session", hosts.overlay))
                .header(header::ORIGIN, &hosts.overlay)
                .json(&json!({"tool": ID, "code": code})),
            StatusCode::OK,
        )
        .await;
        let token = exchange["session"].as_str().unwrap().to_owned();
        assert!(!token.is_empty());
        Self {
            http,
            overlay: hosts.overlay.clone(),
            token,
            jobs: 0,
        }
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.overlay))
            .header(header::ORIGIN, &self.overlay)
            .header("x-vosu-tool-session", &self.token)
    }

    pub async fn action(&mut self) {
        let started = json_response(
            self.request(Method::POST, START)
                .json(&json!({"arguments": {"mode": "wait_progress"}})),
            StatusCode::ACCEPTED,
        )
        .await;
        let id = started["id"].as_str().unwrap();
        assert_eq!(started["action"], ACTION);
        assert_eq!(started["state"], "running");
        let job = format!("/api/v1/jobs/{id}");
        let progress = json_response(
            self.request(Method::GET, &format!("{job}?after=0&wait=1000")),
            StatusCode::OK,
        )
        .await;
        assert_eq!(progress["id"], id);
        assert_eq!(progress["revision"], 1);
        assert_eq!(progress["progress"]["fraction"], 0.375);
        assert_eq!(progress["progress"]["message"], "fixture progress 7913");
        json_response(self.request(Method::DELETE, &job), StatusCode::OK).await;
        let terminal = json_response(
            self.request(Method::GET, &format!("{job}?after=1&wait=1000")),
            StatusCode::OK,
        )
        .await;
        assert_eq!(terminal["id"], id);
        assert_eq!(terminal["revision"], 2);
        assert_eq!(terminal["state"], "cancelled");
        black_box(terminal);
        self.jobs += 1;
    }
}
