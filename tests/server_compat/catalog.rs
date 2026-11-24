// M6b H0 `/api/catalog/*` through the real `routes::dispatch` gate, the real
// `CatalogComposition` (S0/C0/P0/I0/U0/G0 over a real `PluginHost`) and the
// real `CatalogApi`. Included by `tests/server_compat.rs`, sharing its
// imports. The catalog network is an in-memory fixture implementing the
// production `CatalogNetwork` seam; catalogs still pass through S0's one
// SDK-validated `complete_fetch` publication.

mod catalog_fixture {
    use super::*;
    use std::{collections::HashMap, sync::Mutex};
    use vosu::{
        runtime::{CatalogComposition, CatalogRoots, catalog_target},
        server::catalog_api::{CatalogApi, CatalogNetwork},
    };
    use vosu_core::{
        catalog::{
            install::{DownloadFuture, InstallLayout, PackageTransport},
            runtime::{CatalogError, CatalogRuntime},
            sources::FetchedCatalog,
        },
        logging::{ConsoleSink, Logger, LoggerOptions},
        plugin::{
            PluginHost,
            settings::{PluginSettingsService, SettingsError, SettingsStorage},
        },
    };

    pub const USER: &str = "https://catalog.example/user/catalog-v2.json";
    pub const SLASHED: &str = "https://catalog.example/a/b/catalog-v2.json";
    pub const ENCODED: &str = "https://catalog.example/a%2Fb/catalog-v2.json";
    pub const SHORTHAND: &str = "acme/plugins";
    pub const OFFICIAL: &str = "18-lab-dev/vosu-tools";
    pub const TOKEN: &str = "sentinel-token-7f3c91";
    pub const USERNAME: &str = "sentinel-user-52ad";
    pub const PASSWORD: &str = "sentinel-pass-e019";
    pub const DASHBOARD_HOST: &str = "localhost:24051";
    pub const DASHBOARD: &str = "http://localhost:24051";

    struct Storage;
    impl SettingsStorage for Storage {
        fn load(&self, _: &str) -> Result<serde_json::Map<String, Value>, SettingsError> {
            Ok(serde_json::Map::new())
        }
        fn store(&self, _: &str, _: &serde_json::Map<String, Value>) -> Result<(), SettingsError> {
            Ok(())
        }
    }
    struct Console;
    impl ConsoleSink for Console {
        fn is_tty(&self) -> bool {
            false
        }
        fn write_log(&self, _: &str) -> io::Result<()> {
            Ok(())
        }
        fn clear_progress(&self, _: usize) -> io::Result<()> {
            Ok(())
        }
        fn write_progress(&self, _: &str) -> io::Result<()> {
            Ok(())
        }
    }

    /// In-memory catalogs and art; package bytes live in `Packages`.
    #[derive(Default)]
    pub struct Network {
        pub catalogs: Mutex<HashMap<String, Vec<u8>>>,
        pub art: Mutex<HashMap<String, Vec<u8>>>,
        pub art_hits: AtomicUsize,
        pub refreshes: AtomicUsize,
    }
    impl CatalogNetwork for Network {
        fn refresh<'a>(
            &'a self,
            runtime: &'a CatalogRuntime,
            _: &'a CancellationToken,
        ) -> ProviderFuture<'a, Result<(), CatalogError>> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let fetch = runtime.fetch().ok_or(CatalogError::Disabled)?;
                let ticket = fetch.sources().begin_fetch()?;
                let catalogs = self.catalogs.lock().unwrap().clone();
                let fetched = ticket
                    .sources()
                    .iter()
                    .filter(|record| record.enabled)
                    .map(|record| FetchedCatalog {
                        source: record.identity.clone(),
                        result: catalogs.get(record.identity.as_str()).cloned().ok_or(
                            vosu_core::catalog::sources::SourceFailure::CredentialRequired,
                        ),
                    })
                    .collect();
                let inputs = runtime.registration_inputs();
                let facts = runtime.host_facts();
                let published = fetch.sources().complete_fetch(
                    ticket,
                    fetched,
                    &inputs.registrations(),
                    &inputs.revisions(),
                    &facts.application,
                    &facts.target,
                );
                runtime.refresh_offers();
                published.map(|_| ()).map_err(CatalogError::from)
            })
        }
        fn art<'a>(
            &'a self,
            url: &'a str,
            _: &'a CancellationToken,
        ) -> ProviderFuture<'a, Result<(u16, Bytes), String>> {
            self.art_hits.fetch_add(1, Ordering::SeqCst);
            let body = self.art.lock().unwrap().get(url).cloned();
            Box::pin(async move {
                Ok(match body {
                    Some(body) => (200, Bytes::from(body)),
                    None => (404, Bytes::new()),
                })
            })
        }
    }

    #[derive(Default)]
    pub struct Packages {
        pub bodies: Mutex<HashMap<String, Vec<u8>>>,
        pub requests: AtomicUsize,
    }
    impl PackageTransport for Packages {
        fn download<'a>(
            &'a self,
            url: &'a str,
            max_bytes: u64,
            destination: &'a Path,
            _: &'a CancellationToken,
        ) -> DownloadFuture<'a> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            let body = self.bodies.lock().unwrap().get(url).cloned();
            Box::pin(async move {
                let body = body.ok_or(vosu_net::NetworkError::Status(404))?;
                if body.len() as u64 > max_bytes {
                    return Err(vosu_net::NetworkError::TooLarge(max_bytes));
                }
                std::fs::write(destination, body)?;
                Ok(())
            })
        }
    }

    fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            for (name, body) in files {
                writer
                    .start_file(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(body).unwrap();
            }
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn sha256(bytes: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn host_range() -> String {
        format!("={}", vosu::application_version())
    }

    impl Packages {
        fn publish(&self, name: &str, files: &[(&str, &[u8])]) -> Value {
            let archive = zip(files);
            let url = format!("https://packages.example/{name}.zip");
            let package = json!({"url":url,"sha256":sha256(&archive),"size":archive.len()});
            self.bodies.lock().unwrap().insert(url, archive);
            package
        }
        /// A web-only tool package and its exact catalog entry.
        pub fn tool(&self, id: &str) -> Value {
            let manifest = json!({"manifest_version":1,"kind":"tool","id":id,"name":id,
                "version":"1.0.0","host":host_range(),"ui":{"root":"web"}});
            let package = self.publish(
                id,
                &[
                    ("manifest.toml", toml::to_string(&manifest).unwrap().as_bytes()),
                    ("web/index.html", b"<p>tool</p>"),
                ],
            );
            json!({"id":id,"name":id,"version":"1.0.0","host":host_range(),
                "icon_url":format!("https://art.example/{id}.svg"),"packages":{"any":package}})
        }
        /// A native plugin package requiring the built-in `vosu.play` core
        /// namespace (resolved for free, never an add). The bundled osu!
        /// plugin is not loaded here, so the D8 `vosu.osu.internal` case is
        /// proved by the browser smoke against the real daemon.
        pub fn plugin(&self, id: &str, runtime: &str) -> Value {
            let target = catalog_target();
            let library = format!("lib/{id}.so");
            let requires = json!([{"id":"field:vosu.play","version":"*"}]);
            let manifest = json!({"manifest_version":1,"kind":"plugin","id":id,"name":id,
                "version":"1.0.0","host":host_range(),"abi_version":1,"runtime":runtime,
                "targets":{target:{"library":library}},"requires":requires});
            let package = self.publish(
                id,
                &[
                    ("manifest.toml", toml::to_string(&manifest).unwrap().as_bytes()),
                    (library.as_str(), b"fixture library"),
                ],
            );
            json!({"id":id,"name":id,"version":"1.0.0","host":host_range(),
                "runtime":runtime,"requires":requires,"packages":{target:package}})
        }
    }

    pub fn catalog(tools: Vec<Value>, plugins: Vec<Value>) -> Vec<u8> {
        json!({"catalog_version":2,"name":"H0 fixture","published":"2026-11-24T00:00:00Z",
            "tools":tools,"plugins":plugins})
        .to_string()
        .into_bytes()
    }

    pub struct Fixture {
        pub api: CatalogApi,
        pub network: Arc<Network>,
        pub packages: Arc<Packages>,
        pub origins: Arc<vosu::server::origins::Origins>,
        pub composition: CatalogComposition,
        pub base: TempDir,
        _logs: TempDir,
    }

    impl Fixture {
        pub async fn start() -> Self {
            let base = tempfile::tempdir().unwrap();
            let logs = tempfile::tempdir().unwrap();
            let logger = Logger::new(
                logs.path(),
                LoggerOptions {
                    version: "0.1.0".into(),
                    debug_log: false,
                    queue_capacity: 64,
                },
                Arc::new(Console),
            )
            .unwrap();
            let host = PluginHost::new(
                logger,
                Arc::new(PluginSettingsService::new(Arc::new(Storage))),
                tokio::runtime::Handle::current(),
            );
            let root = base.path();
            let packages = Arc::new(Packages::default());
            let composition = bounded!(CatalogComposition::start(
                host,
                CatalogRoots {
                    config: root.join("config"),
                    layout: InstallLayout::new(
                        root.join("catalog"),
                        root.join("catalog/plugins"),
                        root.join("tools"),
                        root.join("static"),
                        root.join("downloads"),
                    ),
                    plugin_data: root.join("plugin-data"),
                },
                packages.clone(),
            ))
            .unwrap();
            let network = Arc::new(Network::default());
            let api = composition.api(Arc::new(|| Some(512 * 1024 * 1024)), network.clone());
            let origins = Arc::new(
                vosu::server::origins::Origins::new(
                    "http://127.0.0.1:24050".into(),
                    "http://127.0.0.1:24051".into(),
                )
                .unwrap(),
            );
            Self {
                api,
                network,
                packages,
                origins,
                composition,
                base,
                _logs: logs,
            }
        }

        /// One request through the production dispatch gate on the dashboard
        /// (`dashboard`) or overlay listener. `origin`/`csrf` are sent only
        /// when given; `extra` adds raw headers.
        pub async fn send(
            &self,
            dashboard: bool,
            method: Method,
            uri: &str,
            origin: Option<&str>,
            csrf: Option<&str>,
            extra: &[(&str, &str)],
            body: Value,
        ) -> (StatusCode, axum::http::HeaderMap, String) {
            struct Delegate(CatalogApi);
            impl RouteDelegate for Delegate {
                fn handle(
                    &self,
                    endpoint: Endpoint,
                    request: Request<Body>,
                    body: Bytes,
                ) -> ProviderFuture<'_, Response> {
                    assert_eq!(endpoint, Endpoint::Catalog, "only catalog routes here");
                    self.0.handle(request, body)
                }
            }
            let mut providers = providers();
            providers.delegate = Arc::new(Delegate(self.api.clone()));
            providers.authorization = Arc::new(vosu::server::origins::OriginAuthorizer {
                inner: providers.authorization,
                origins: Arc::clone(&self.origins),
                dashboard,
            });
            let mut request = Request::builder().method(method).uri(uri).header(
                header::HOST,
                if dashboard {
                    DASHBOARD_HOST
                } else {
                    "localhost:24050"
                },
            );
            if let Some(origin) = origin {
                request = request.header(header::ORIGIN, origin);
            }
            if let Some(csrf) = csrf {
                request = request.header("X-Vosu-CSRF", csrf);
            }
            for (name, value) in extra {
                request = request.header(*name, *value);
            }
            let body = if body.is_null() {
                Body::empty()
            } else {
                Body::from(body.to_string())
            };
            let mut request = request.body(body).unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(PeerAddress(SocketAddr::from((
                    [127, 0, 0, 1],
                    40000,
                )))));
            let response = bounded!(routes::dispatch(State(providers), request));
            let status = response.status();
            let headers = response.headers().clone();
            let body = bounded!(to_bytes(response.into_body(), 4 * 1024 * 1024)).unwrap();
            (status, headers, String::from_utf8(body.to_vec()).unwrap())
        }
        /// Trusted dashboard request: dashboard listener, origin and CSRF.
        pub async fn dashboard(&self, method: Method, uri: &str, body: Value) -> (StatusCode, String) {
            let csrf = self.origins.csrf.clone();
            let (status, _, body) = self
                .send(true, method, uri, Some(DASHBOARD), Some(&csrf), &[], body)
                .await;
            (status, body)
        }
        pub async fn json(&self, method: Method, uri: &str, body: Value) -> Value {
            let (status, text) = self.dashboard(method, uri, body).await;
            assert!(status.is_success(), "{uri}: {status} {text}");
            serde_json::from_str(&text).unwrap()
        }
        pub async fn sources(&self, list: &[(&str, bool)]) -> Value {
            let sources: Vec<Value> = list
                .iter()
                .map(|(source, enabled)| json!({"source":source,"enabled":enabled}))
                .collect();
            self.json(Method::POST, "/api/catalog/sources", json!({"sources":sources}))
                .await
        }
        pub fn record<'a>(view: &'a Value, source: &str) -> &'a Value {
            view["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|record| record["source"] == source)
                .unwrap_or_else(|| panic!("{source} not listed in {view}"))
        }
        pub async fn wait_job(&self, job: &str) -> Value {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            loop {
                let state = self
                    .json(Method::GET, &format!("/api/catalog/jobs/{job}"), Value::Null)
                    .await;
                if state["status"] != "running" {
                    return state;
                }
                assert!(tokio::time::Instant::now() < deadline, "job {job} hung");
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        pub async fn shutdown(self) {
            bounded!(self.composition.shutdown());
        }
    }

    /// The dashboard's `encodeURIComponent` of one source segment.
    pub fn component(value: &str) -> String {
        url::form_urlencoded::byte_serialize(value.as_bytes())
            .collect::<String>()
            .replace('+', "%20")
    }
}

use catalog_fixture::*;

#[tokio::test]
async fn catalog_routes_are_dashboard_only_exact_and_csrf_gated_before_side_effects() {
    let fixture = Fixture::start().await;
    fixture.sources(&[(OFFICIAL, true), (USER, true)]).await;
    let trust = format!("/api/catalog/sources/{}/trust", component(USER));
    let csrf = fixture.origins.csrf.clone();
    // Overlay listener: every catalog route is absent, mutations included,
    // whatever origin/CSRF it presents (overlay/dashboard port inversion).
    for (method, uri) in [
        (Method::GET, "/api/catalog/sources".to_owned()),
        (Method::GET, "/api/catalog/status".to_owned()),
        (Method::POST, trust.clone()),
        (Method::DELETE, format!("/api/catalog/sources/{}/credential", component(USER))),
    ] {
        for origin in [Some("http://localhost:24050"), Some(DASHBOARD)] {
            let (status, _, _) = fixture
                .send(false, method.clone(), &uri, origin, Some(&csrf), &[], json!({"trusted":true}))
                .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri} via overlay listener");
        }
    }
    // Dashboard listener mutations without exact provenance + CSRF, or with a
    // key/bearer/tool token instead, never reach the handler.
    let refusals: [(Option<&str>, Option<&str>, &[(&str, &str)]); 7] = [
        (Some(DASHBOARD), None, &[]),
        (Some(DASHBOARD), Some("wrong-csrf"), &[]),
        (None, Some(csrf.as_str()), &[]),
        (Some("http://localhost:24050"), Some(csrf.as_str()), &[]),
        (Some("null"), Some(csrf.as_str()), &[]),
        (Some(DASHBOARD), None, &[("Authorization", concat!("Bear", "er", " fixture-key"))]),
        (Some(DASHBOARD), None, &[("X-Vosu-Tool-Token", "tool-token")]),
    ];
    for (origin, token, extra) in refusals {
        for (method, uri, body) in [
            (Method::POST, trust.clone(), json!({"trusted":true})),
            (
                Method::POST,
                format!("/api/catalog/sources/{}/credential", component(USER)),
                json!({"kind":"basic","username":USERNAME,"password":PASSWORD}),
            ),
            (Method::POST, "/api/catalog/refresh".into(), Value::Null),
        ] {
            let (status, _, _) = fixture
                .send(true, method.clone(), &uri, origin, token, extra, body)
                .await;
            assert!(
                matches!(status, StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED),
                "{method} {uri} {origin:?} {token:?} {extra:?}: {status}"
            );
        }
    }
    let view = fixture
        .json(Method::GET, "/api/catalog/sources", Value::Null)
        .await;
    assert_eq!(Fixture::record(&view, USER)["trusted"], false);
    assert_eq!(Fixture::record(&view, USER)["has_credential"], false);
    assert_eq!(fixture.network.refreshes.load(Ordering::SeqCst), 0);
    // A credential exists, then DELETE without CSRF is refused and keeps it.
    let credential = format!("/api/catalog/sources/{}/credential", component(USER));
    let basic = json!({"kind":"basic","username":USERNAME,"password":PASSWORD});
    assert_eq!(
        fixture.dashboard(Method::POST, &credential, basic).await.0,
        StatusCode::NO_CONTENT
    );
    let (status, _, _) = fixture
        .send(true, Method::DELETE, &credential, Some(DASHBOARD), None, &[], Value::Null)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let presence = fixture.json(Method::GET, &credential, Value::Null).await;
    assert_eq!(presence["has_credential"], true);

    // Exact paths and methods: near misses are 404, wrong methods 405.
    for (method, uri, expected) in [
        (Method::GET, "/api/catalog/sources/".to_owned(), StatusCode::NOT_FOUND),
        (Method::GET, "/api/catalog".to_owned(), StatusCode::NOT_FOUND),
        (Method::GET, "/api/catalog/Sources".to_owned(), StatusCode::NOT_FOUND),
        (Method::GET, "/api/catalog/status/".to_owned(), StatusCode::NOT_FOUND),
        (Method::PUT, "/api/catalog/sources".to_owned(), StatusCode::NOT_FOUND),
        (Method::PATCH, trust.clone(), StatusCode::NOT_FOUND),
        (Method::HEAD, "/api/catalog/status".to_owned(), StatusCode::NOT_FOUND),
        (Method::GET, "/api/catalog/install".to_owned(), StatusCode::METHOD_NOT_ALLOWED),
        (Method::GET, trust.clone(), StatusCode::METHOD_NOT_ALLOWED),
        (Method::DELETE, "/api/catalog/sources".to_owned(), StatusCode::METHOD_NOT_ALLOWED),
        (Method::POST, "/api/catalog/status".to_owned(), StatusCode::METHOD_NOT_ALLOWED),
        (
            Method::POST,
            format!("/api/catalog/sources/{}/trust/", component(USER)),
            StatusCode::NOT_FOUND,
        ),
        (
            Method::POST,
            "/api/catalog/sources/https:%2F%2Fcatalog.example%2Fuser%2Fcatalog-v2.json%zz/trust"
                .to_owned(),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, body) = fixture.dashboard(method.clone(), &uri, json!({"trusted":true})).await;
        assert_eq!(status, expected, "{method} {uri}: {body}");
    }
    let view = fixture
        .json(Method::GET, "/api/catalog/sources", Value::Null)
        .await;
    assert_eq!(Fixture::record(&view, USER)["trusted"], false);
    fixture.shutdown().await;
}

#[tokio::test]
async fn list_replacement_cannot_smuggle_trust_or_private_network_and_removal_resets_flags() {
    let fixture = Fixture::start().await;
    fixture.sources(&[(OFFICIAL, true), (USER, true)]).await;
    for smuggled in [
        json!({"sources":[{"source":USER,"enabled":true,"trusted":true}]}),
        json!({"sources":[{"source":USER,"enabled":true,"private_network":true}]}),
        json!({"sources":[{"source":USER,"enabled":true}],"trusted":[USER]}),
        json!({"sources":[{"source":USER,"enabled":true,"order":0}]}),
    ] {
        let (status, body) = fixture
            .dashboard(Method::POST, "/api/catalog/sources", smuggled)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["error"], "invalid request body");
    }
    let view = fixture
        .json(Method::GET, "/api/catalog/sources", Value::Null)
        .await;
    assert_eq!(Fixture::record(&view, USER)["trusted"], false);
    assert_eq!(Fixture::record(&view, USER)["private_network"], false);
    // Order is the array order; enabled flags persist exactly.
    let view = fixture.sources(&[(USER, false), (OFFICIAL, true)]).await;
    assert_eq!(Fixture::record(&view, USER)["order"], 0);
    assert_eq!(Fixture::record(&view, USER)["enabled"], false);
    assert_eq!(Fixture::record(&view, OFFICIAL)["order"], 1);
    // Dedicated routes change the flags; a client `warning_shown` claim is
    // not authority and is refused like any unknown field.
    let trust = format!("/api/catalog/sources/{}/trust", component(USER));
    let private = format!("/api/catalog/sources/{}/private-network", component(USER));
    assert_eq!(
        fixture
            .dashboard(Method::POST, &trust, json!({"trusted":true,"warning_shown":true}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let view = fixture.json(Method::POST, &trust, json!({"trusted":true})).await;
    assert_eq!(Fixture::record(&view, USER)["trusted"], true);
    let view = fixture.json(Method::POST, &private, json!({"allowed":true})).await;
    assert_eq!(Fixture::record(&view, USER)["private_network"], true);
    // Replacement keeps the flags of kept identities and never adds any.
    let view = fixture.sources(&[(OFFICIAL, true), (USER, true)]).await;
    assert_eq!(Fixture::record(&view, USER)["trusted"], true);
    // Officials are always trusted and never get a private exception.
    let official = component(OFFICIAL);
    for (uri, body) in [
        (format!("/api/catalog/sources/{official}/trust"), json!({"trusted":false})),
        (format!("/api/catalog/sources/{official}/private-network"), json!({"allowed":true})),
    ] {
        assert_eq!(fixture.dashboard(Method::POST, &uri, body).await.0, StatusCode::CONFLICT);
    }
    // Removal purges trust/private/credential; re-adding starts clean.
    let credential = format!("/api/catalog/sources/{}/credential", component(USER));
    let basic = json!({"kind":"basic","username":USERNAME,"password":PASSWORD});
    assert_eq!(
        fixture.dashboard(Method::POST, &credential, basic).await.0,
        StatusCode::NO_CONTENT
    );
    fixture.sources(&[(OFFICIAL, true)]).await;
    let view = fixture.sources(&[(OFFICIAL, true), (USER, true)]).await;
    let record = Fixture::record(&view, USER);
    assert_eq!(
        (&record["trusted"], &record["private_network"], &record["has_credential"]),
        (&json!(false), &json!(false), &json!(false))
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn credentials_are_write_only_and_keyed_by_the_exactly_once_decoded_source() {
    let fixture = Fixture::start().await;
    fixture
        .sources(&[(SLASHED, true), (ENCODED, true), (SHORTHAND, true), (USER, true)])
        .await;
    // The encoded segment of `…/a%2Fb/…` decodes once to that exact string;
    // a double decoder would have reached `…/a/b/…` instead.
    let encoded = format!("/api/catalog/sources/{}/credential", component(ENCODED));
    assert!(encoded.contains("a%252Fb"));
    let (status, headers, body) = fixture
        .send(
            true,
            Method::POST,
            &encoded,
            Some(DASHBOARD),
            Some(&fixture.origins.csrf.clone()),
            &[],
            json!({"kind":"basic","username":USERNAME,"password":PASSWORD}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(body.is_empty());
    let view = fixture
        .json(Method::GET, "/api/catalog/sources", Value::Null)
        .await;
    assert_eq!(Fixture::record(&view, ENCODED)["has_credential"], true);
    assert_eq!(Fixture::record(&view, SLASHED)["has_credential"], false);
    // GitHub shorthand: a raw `/` never splits a source, and one extra
    // encoding level names a different (unconfigured) identity.
    let token = json!({"kind":"token","token":TOKEN});
    for raw in [
        "/api/catalog/sources/acme/plugins/credential",
        "/api/catalog/sources/acme%252Fplugins/credential",
    ] {
        assert_eq!(
            fixture.dashboard(Method::POST, raw, token.clone()).await.0,
            StatusCode::NOT_FOUND,
            "{raw}"
        );
    }
    let view = fixture
        .json(Method::GET, "/api/catalog/sources", Value::Null)
        .await;
    assert_eq!(Fixture::record(&view, SHORTHAND)["has_credential"], false);
    let shorthand = format!("/api/catalog/sources/{}/credential", component(SHORTHAND));
    assert_eq!(
        fixture.dashboard(Method::POST, &shorthand, token).await.0,
        StatusCode::NO_CONTENT
    );
    // Static sources take Basic only; a token there and malformed bodies are
    // a fixed 400 that never echoes the submitted secret.
    for (uri, body) in [
        (encoded.clone(), json!({"kind":"token","token":TOKEN})),
        (encoded.clone(), json!({"kind":"basic","username":USERNAME,"password":PASSWORD,"x":1})),
        (shorthand.clone(), json!({"kind":"token","token":format!("{TOKEN}\r\nX: y")})),
    ] {
        let (status, body) = fixture.dashboard(Method::POST, &uri, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        for secret in [TOKEN, USERNAME, PASSWORD] {
            assert!(!body.contains(secret), "{body}");
        }
    }
    // Presence only, from every read surface.
    fixture.network.catalogs.lock().unwrap().insert(
        USER.into(),
        catalog(vec![fixture.packages.tool("plain")], Vec::new()),
    );
    let mut surfaces = Vec::new();
    for uri in [
        "/api/catalog/sources".to_owned(),
        shorthand.clone(),
        encoded.clone(),
        "/api/catalog/status".into(),
        "/api/catalog/entries".into(),
        "/api/catalog/installed".into(),
        "/api/catalog/offers".into(),
        "/api/catalog/art?url=https%3A%2F%2Fart.example%2Fmissing.svg".into(),
        "/api/catalog/jobs/unknown".into(),
    ] {
        surfaces.push(fixture.dashboard(Method::GET, &uri, Value::Null).await.1);
    }
    surfaces.push(
        fixture
            .dashboard(Method::POST, "/api/catalog/refresh", Value::Null)
            .await
            .1,
    );
    let presence: Value = serde_json::from_str(&surfaces[1]).unwrap();
    assert_eq!(presence, json!({"has_credential":true,"available":true}));
    for surface in &surfaces {
        for secret in [TOKEN, USERNAME, PASSWORD] {
            assert!(!surface.contains(secret), "{secret} leaked: {surface}");
        }
    }
    // The secret is persisted for upstream use, never anywhere else in config.
    let stored = std::fs::read_to_string(
        fixture.base.path().join("config/catalog-credentials.toml"),
    )
    .unwrap();
    assert!(stored.contains(TOKEN) && stored.contains(PASSWORD));
    for entry in std::fs::read_dir(fixture.base.path().join("config")).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap() != "catalog-credentials.toml" && path.is_file() {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(!text.contains(TOKEN) && !text.contains(PASSWORD), "{path:?}");
        }
    }
    let (status, headers, _) = fixture
        .send(
            true,
            Method::DELETE,
            &shorthand,
            Some(DASHBOARD),
            Some(&fixture.origins.csrf.clone()),
            &[],
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let presence = fixture.json(Method::GET, &shorthand, Value::Null).await;
    assert_eq!(presence["has_credential"], false);
    fixture.shutdown().await;
}

