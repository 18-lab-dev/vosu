// M6b H0 install plans, confirmation, jobs, load reports, uninstall and
// listed art through `/api/catalog/*`; and the removed 1.x immediate-install
// routes. Included by `tests/server_compat.rs` after `catalog.rs`.

#[tokio::test]
async fn install_plans_review_native_runtime_and_approval_then_confirm_once() {
    let fixture = Fixture::start().await;
    fixture.sources(&[(OFFICIAL, true), (USER, true)]).await;
    let tool = fixture.packages.tool("skill-tracker");
    let process = fixture.packages.plugin("acme.proc", "process");
    let native = fixture.packages.plugin("acme.native", "in-process");
    fixture.network.catalogs.lock().unwrap().insert(
        USER.into(),
        catalog(vec![tool], vec![process, native]),
    );
    // No plan before the sources were fetched.
    let request = |kind: &str, id: &str| json!({"kind":kind,"id":id,"source":USER});
    let (status, body) = fixture
        .dashboard(Method::POST, "/api/catalog/install", request("tool", "skill-tracker"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let view = fixture
        .json(Method::POST, "/api/catalog/refresh", Value::Null)
        .await;
    assert_eq!(Fixture::record(&view, USER)["status"], "ok", "{view}");
    assert_eq!(Fixture::record(&view, USER)["entries"], 3);
    let entries = fixture
        .json(Method::GET, "/api/catalog/entries", Value::Null)
        .await;
    let ids: Vec<&str> = entries["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["acme.proc", "acme.native", "skill-tracker"]);
    assert_eq!(fixture.packages.requests.load(Ordering::SeqCst), 0);

    // An untrusted in-process plugin is a reviewable error plan, never
    // downgraded; it cannot be confirmed.
    let plan = fixture
        .json(Method::POST, "/api/catalog/install", request("plugin", "acme.native"))
        .await;
    assert_eq!(plan["adds"], json!([]));
    assert_eq!(plan["errors"].as_array().unwrap().len(), 1, "{plan}");
    let confirm = |plan: &Value| {
        format!(
            "/api/catalog/plans/{}/confirm",
            plan["plan_id"].as_str().unwrap()
        )
    };
    assert_eq!(
        fixture
            .dashboard(Method::POST, &confirm(&plan), json!({"approve":[]}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );

    // A user process plugin: native, needs approval, D8 disclosed.
    let plan = fixture
        .json(Method::POST, "/api/catalog/install", request("plugin", "acme.proc"))
        .await;
    assert_eq!(
        plan["adds"],
        json!([{"id":"acme.proc","kind":"plugin","source":USER,"version":"1.0.0",
            "reason":"requested","native":true,"trust":"user","runtime":"process",
            "needs_approval":true,"requires_internal_paths":false}])
    );
    assert_eq!(plan["errors"], json!([]));
    for approve in [json!([]), json!(["skill-tracker"]), json!(["acme.proc", "acme.proc"])] {
        let (status, body) = fixture
            .dashboard(Method::POST, &confirm(&plan), json!({"approve":approve}))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{approve}: {body}");
    }
    // Stale: a trust change after review refuses the old plan (no recompute).
    let stale = plan.clone();
    let trust = format!("/api/catalog/sources/{}/trust", component(USER));
    fixture.json(Method::POST, &trust, json!({"trusted":true})).await;
    fixture.json(Method::POST, &trust, json!({"trusted":false})).await;
    assert_eq!(
        fixture
            .dashboard(Method::POST, &confirm(&stale), json!({"approve":["acme.proc"]}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(fixture.packages.requests.load(Ordering::SeqCst), 0);
    fixture
        .json(Method::POST, "/api/catalog/refresh", Value::Null)
        .await;
    let plan = fixture
        .json(Method::POST, "/api/catalog/install", request("plugin", "acme.proc"))
        .await;
    let job = fixture
        .json(Method::POST, &confirm(&plan), json!({"approve":["acme.proc"]}))
        .await;
    let state = fixture.wait_job(job["job_id"].as_str().unwrap()).await;
    assert_eq!(state["status"], "completed", "{state}");
    assert_eq!(
        state.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["id", "status", "phase", "tool", "error"]
    );
    // Single use: the consumed plan is refused, unknown ids are 404.
    assert_eq!(
        fixture
            .dashboard(Method::POST, &confirm(&plan), json!({"approve":["acme.proc"]}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        fixture
            .dashboard(Method::POST, "/api/catalog/plans/unknown/confirm", json!({"approve":[]}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // The load outcome the job envelope cannot carry is in the status.
    let status = fixture
        .json(Method::GET, "/api/catalog/status", Value::Null)
        .await;
    assert_eq!(
        status["load_reports"],
        json!([{"kind":"plugin","id":"acme.proc","status":"requires-process-runtime",
            "detail":"installed; the process plugin runtime is not available in this build"}])
    );
    let installed = fixture
        .json(Method::GET, "/api/catalog/installed", Value::Null)
        .await;
    assert_eq!(
        installed["packages"],
        json!([{"kind":"plugin","id":"acme.proc","version":"1.0.0","source":USER,
            "official":false,"installed_for":"requested","runtime":"process","approved":true}])
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn uninstall_preview_start_and_promote_share_the_operation_gate() {
    let fixture = Fixture::start().await;
    fixture.sources(&[(OFFICIAL, true), (USER, true)]).await;
    let tool = fixture.packages.tool("skill-tracker");
    fixture
        .network
        .catalogs
        .lock()
        .unwrap()
        .insert(USER.into(), catalog(vec![tool], Vec::new()));
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
    assert_eq!(plan["adds"][0]["native"], false);
    assert_eq!(plan["adds"][0]["trust"], Value::Null);
    let id = plan["plan_id"].as_str().unwrap();
    let job = fixture
        .json(
            Method::POST,
            &format!("/api/catalog/plans/{id}/confirm"),
            json!({}),
        )
        .await;
    let state = fixture.wait_job(job["job_id"].as_str().unwrap()).await;
    assert_eq!(state["status"], "completed", "{state}");
    assert!(fixture.base.path().join("tools/skill-tracker/web/index.html").is_file());
    let target = json!({"kind":"tool","id":"skill-tracker","source":USER});
    // Requested already: promotion is a records-only no-op.
    let promoted = fixture
        .json(Method::POST, "/api/catalog/promote", target.clone())
        .await;
    assert_eq!(promoted, json!({"promoted":false}));
    // Exact source identity: another spelling is not installed.
    let (status, _) = fixture
        .dashboard(
            Method::POST,
            "/api/catalog/uninstall/preview",
            json!({"kind":"tool","id":"skill-tracker","source":"acme/plugins"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let preview = fixture
        .json(Method::POST, "/api/catalog/uninstall/preview", target.clone())
        .await;
    assert_eq!(
        preview,
        json!({"removals":[{"kind":"tool","id":"skill-tracker","version":"1.0.0",
            "source":USER,"reason":"explicit"}],"dropped_optional":[]})
    );
    // `remove_data` is an explicit choice.
    assert_eq!(
        fixture
            .dashboard(Method::POST, "/api/catalog/uninstall", target.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut request = target.clone();
    request["remove_data"] = json!(false);
    let job = fixture
        .json(Method::POST, "/api/catalog/uninstall", request)
        .await;
    let state = fixture.wait_job(job["job_id"].as_str().unwrap()).await;
    assert_eq!(state["status"], "completed", "{state}");
    assert!(!fixture.base.path().join("tools/skill-tracker").exists());
    let installed = fixture
        .json(Method::GET, "/api/catalog/installed", Value::Null)
        .await;
    assert_eq!(installed["packages"], json!([]));
    fixture.shutdown().await;
}

#[tokio::test]
async fn listed_art_is_proxied_typed_cached_per_snapshot_and_inert() {
    const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"></svg>"#;
    let fixture = Fixture::start().await;
    fixture.sources(&[(USER, true)]).await;
    let tool = fixture.packages.tool("artful");
    let icon = tool["icon_url"].as_str().unwrap().to_owned();
    let mut page = fixture.packages.tool("page");
    page["icon_url"] = json!("https://art.example/page.svg");
    fixture.network.catalogs.lock().unwrap().insert(
        USER.into(),
        catalog(vec![tool, page], Vec::new()),
    );
    fixture.network.art.lock().unwrap().extend([
        (icon.clone(), SVG.to_vec()),
        ("https://art.example/page.svg".into(), b"<html></html>".to_vec()),
        ("https://art.example/unlisted.svg".into(), SVG.to_vec()),
    ]);
    let art = |url: &str| format!("/api/catalog/art?url={}", component(url));
    // Nothing is listed before a snapshot exists.
    assert_eq!(fixture.dashboard(Method::GET, &art(&icon), Value::Null).await.0, StatusCode::NOT_FOUND);
    fixture
        .json(Method::POST, "/api/catalog/refresh", Value::Null)
        .await;
    let inert = |headers: &axum::http::HeaderMap| {
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:"
        );
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers["cross-origin-resource-policy"], "same-origin");
        assert_eq!(headers[header::CONTENT_DISPOSITION], "inline");
        assert!(headers.get(header::SET_COOKIE).is_none());
    };
    for _ in 0..2 {
        let (status, headers, body) = fixture
            .send(true, Method::GET, &art(&icon), None, None, &[], Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "image/svg+xml");
        assert_eq!(body.as_bytes(), SVG);
        inert(&headers);
    }
    assert_eq!(fixture.network.art_hits.load(Ordering::SeqCst), 1);
    // A new publication (same generation) is a new cache epoch.
    fixture
        .json(Method::POST, "/api/catalog/refresh", Value::Null)
        .await;
    assert_eq!(fixture.dashboard(Method::GET, &art(&icon), Value::Null).await.0, StatusCode::OK);
    assert_eq!(fixture.network.art_hits.load(Ordering::SeqCst), 2);
    // Unlisted URLs are never fetched; listed non-images are 502.
    for (url, expected) in [
        ("https://art.example/unlisted.svg", StatusCode::NOT_FOUND),
        ("https://art.example/page.svg", StatusCode::BAD_GATEWAY),
    ] {
        let (status, headers, _) = fixture
            .send(true, Method::GET, &art(url), None, None, &[], Value::Null)
            .await;
        assert_eq!(status, expected, "{url}");
        inert(&headers);
    }
    assert_eq!(fixture.network.art_hits.load(Ordering::SeqCst), 3);
    fixture.shutdown().await;
}

#[tokio::test]
async fn removed_immediate_install_routes_never_install_or_start_a_job() {
    let root = tempfile::tempdir().unwrap();
    let host = bounded!(vosu::tools::ToolRegistry::new(
        root.path().join("config"),
        root.path().join("tools"),
        root.path().join("cache"),
        vosu::tools::ExtraRoots { dev: Vec::new() },
        "http://127.0.0.1:24050".into(),
        Some("http://127.0.0.1:24051".into()),
    ))
    .unwrap();
    for (method, path) in [
        (Method::POST, "/api/tools/fixture/install"),
        (Method::POST, "/api/tools/fixture/update"),
        (Method::POST, "/api/tools/fixture/uninstall"),
        (Method::POST, "/api/tools/catalog/overlays/fixture/install"),
        (Method::GET, "/api/tools/catalog"),
        (Method::GET, "/api/tools/catalog/sources"),
        (Method::POST, "/api/tools/catalog/sources"),
        (Method::GET, "/api/tools/catalog/jobs/x"),
        (Method::GET, "/api/tools/catalog/art"),
    ] {
        let request = Request::builder()
            .method(method.clone())
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = bounded!(host.dashboard(request, Bytes::from_static(b"{}")));
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{method} {path}");
        let body = bounded!(to_bytes(response.into_body(), 4096)).unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("job_id"), "{path}");
    }
    assert_eq!(std::fs::read_dir(root.path().join("tools")).unwrap().count(), 0);
    bounded!(host.shutdown());
}
