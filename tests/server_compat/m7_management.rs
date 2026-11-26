// M7 D1 management counterexamples over the production catalog routes:
// `ToolView.update_available` from core's catalog update source, and a
// source the user never added or that could not be fetched yields no plan.
// Included by `tests/server_compat.rs` after `catalog_plans.rs`.

async fn tool_view(tools: &Arc<vosu_core::tools::ToolRegistry>, id: &str) -> Value {
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("/api/tools/{id}"))
        .body(Body::empty())
        .unwrap();
    let response = bounded!(tools.dashboard(request, Bytes::new()));
    assert_eq!(response.status(), StatusCode::OK);
    let body = bounded!(to_bytes(response.into_body(), 1 << 20)).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn d1_tool_update_available_comes_from_its_own_catalog_source() {
    let fixture = Fixture::start().await;
    let root = fixture.base.path();
    let tools = bounded!(vosu_core::tools::ToolRegistry::new(
        root.join("tool-config"),
        root.join("tools"),
        root.join("tool-cache"),
        vosu_core::tools::ExtraRoots { dev: Vec::new() },
        "http://127.0.0.1:24050".into(),
        Some("http://127.0.0.1:24051".into()),
        vosu::BUNDLE.version(),
    ))
    .unwrap();
    fixture.composition.attach_tools(Arc::clone(&tools));
    fixture.sources(&[(OFFICIAL, true), (USER, true)]).await;
    let listed = fixture.packages.tool("skill-tracker");
    let publish = |entry: &Value| {
        fixture
            .network
            .catalogs
            .lock()
            .unwrap()
            .insert(USER.into(), catalog(vec![entry.clone()], Vec::new()));
    };
    publish(&listed);
    fixture
        .json(Method::POST, "/api/catalog/refresh", Value::Null)
        .await;
    let plan = fixture
        .json(
            Method::POST,
            "/api/catalog/install",
            json!({"kind":"tool","id":"skill-tracker","source":USER}),
        )
        .await;
    let job = fixture
        .json(
            Method::POST,
            &format!("/api/catalog/plans/{}/confirm", plan["plan_id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    let state = fixture.wait_job(job["job_id"].as_str().unwrap()).await;
    assert_eq!(state["status"], "completed", "{state}");
    assert_eq!(tool_view(&tools, "skill-tracker").await["update_available"], false);

    // A newer listing from the exact installed source is an update; an
    // older one and one this host cannot install are not.
    for (version, host, expected) in [
        ("1.1.0", listed["host"].clone(), true),
        ("0.9.0", listed["host"].clone(), false),
        ("1.2.0", json!("=0.0.1"), false),
    ] {
        let mut entry = listed.clone();
        entry["version"] = json!(version);
        entry["host"] = host;
        publish(&entry);
        fixture
            .json(Method::POST, "/api/catalog/refresh", Value::Null)
            .await;
        assert_eq!(
            tool_view(&tools, "skill-tracker").await["update_available"],
            expected,
            "{version}"
        );
    }
    // The same id listed newer by another source is not this tool's update.
    let mut elsewhere = listed.clone();
    elsewhere["version"] = json!("9.0.0");
    fixture
        .network
        .catalogs
        .lock()
        .unwrap()
        .insert(USER.into(), catalog(Vec::new(), Vec::new()));
    fixture.sources(&[(OFFICIAL, true), (USER, true), (SHORTHAND, true)]).await;
    fixture
        .network
        .catalogs
        .lock()
        .unwrap()
        .insert(SHORTHAND.into(), catalog(vec![elsewhere], Vec::new()));
    fixture
        .json(Method::POST, "/api/catalog/refresh", Value::Null)
        .await;
    assert_eq!(tool_view(&tools, "skill-tracker").await["update_available"], false);
    bounded!(tools.shutdown());
    fixture.shutdown().await;
}

/// D1 counterexample: a missing source reveals no plan. A source the user
/// never added, one added but never fetched, and one whose fetch failed
/// never produce an installable plan: at most an error plan naming the
/// unavailable item that cannot be confirmed, and nothing is downloaded.
#[tokio::test]
async fn d1_a_missing_or_unfetched_source_never_reveals_a_plan() {
    let fixture = Fixture::start().await;
    fixture.sources(&[(OFFICIAL, true), (USER, true)]).await;
    let request = |source: &str| json!({"kind":"tool","id":"skill-tracker","source":source});
    let (status, body) = fixture
        .dashboard(Method::POST, "/api/catalog/install", request(USER))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "never fetched: {body}");
    assert!(!body.contains("plan_id"), "{body}");
    // USER's fetch fails: no catalog is published for it.
    for source in [USER, SHORTHAND, "https://catalog.example/never-added/catalog-v2.json"] {
        let (status, body) = fixture
            .dashboard(Method::POST, "/api/catalog/install", request(source))
            .await;
        if status.is_client_error() {
            assert!(!body.contains("plan_id"), "{source}: {body}");
            continue;
        }
        assert_eq!(status, StatusCode::OK, "{source}: {body}");
        let plan: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(plan["adds"], json!([]), "{source}: {plan}");
        assert!(!plan["errors"].as_array().unwrap().is_empty(), "{source}: {plan}");
        let confirm = format!(
            "/api/catalog/plans/{}/confirm",
            plan["plan_id"].as_str().unwrap()
        );
        let (status, _) = fixture
            .dashboard(Method::POST, &confirm, json!({"approve":[]}))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{source}");
    }
    assert_eq!(fixture.packages.requests.load(Ordering::SeqCst), 0);
    fixture.shutdown().await;
}
