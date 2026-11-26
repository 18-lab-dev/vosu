// The design-system set is the tool host's registration on both listener
// roles and the dashboard set the dashboard host's, dashboard listener only.
// Core embeds and serves neither, so the retired root files are 404 (A7).
async fn assert_design_system_routes(client: &reqwest::Client, base: &str, management: &str) {
    for origin in [base, management] {
        let css = bounded!(client.get(format!("{origin}/assets/tokens.css")).send()).unwrap();
        assert_eq!(css.status(), 200, "{origin}");
        assert_eq!(
            css.headers()[reqwest::header::CONTENT_TYPE],
            "text/css; charset=utf-8"
        );
        assert!(!bounded!(css.bytes()).unwrap().is_empty());
        for retired in [
            "/assets/tool-bridge.js",
            "/assets/design.html",
            "/assets/..%2FLICENSE",
        ] {
            let response = bounded!(client.get(format!("{origin}{retired}")).send()).unwrap();
            assert_eq!(response.status(), 404, "{origin}{retired}");
        }
        let favicon = bounded!(client.get(format!("{origin}/favicon.ico")).send()).unwrap();
        assert_eq!(favicon.status(), 200, "{origin}");
        assert_eq!(favicon.headers()["x-vosu-identity"], "vosu", "{origin}");
    }
    let overlay_dashboard_css =
        bounded!(client.get(format!("{base}/assets/homepage.css")).send()).unwrap();
    assert_eq!(overlay_dashboard_css.status(), 404);
    let css = bounded!(
        client
            .get(format!("{management}/assets/homepage.css"))
            .send()
    )
    .unwrap();
    assert_eq!(css.status(), 200);
    assert!(!bounded!(css.bytes()).unwrap().is_empty());
    // The second-launch probe recognises this daemon by its favicon header.
    let port = reqwest::Url::parse(management)
        .unwrap()
        .port()
        .expect("explicit dashboard port");
    assert_eq!(
        bounded!(vosu_core::desktop::existing_dashboard("127.0.0.1", port)),
        Some(vosu_core::desktop::dashboard_url("127.0.0.1", port)),
    );
}
