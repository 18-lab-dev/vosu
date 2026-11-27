use crate::page::{self, CONTEXT_MARKER, Page};
use crate::routes::Routes;
use vosu_surface::{
    listener::ListenerRole,
    routes::{ResponseBody, RoutePath},
};

fn query(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn body(routes: &Routes, path: &str, pairs: &[(&str, &str)]) -> (String, String) {
    let response = routes.response(path, &query(pairs)).expect(path);
    assert_eq!(response.status, 200);
    assert!(response.valid_metadata());
    let ResponseBody::Bytes(bytes) = response.body else {
        panic!("{path}: expected bytes");
    };
    (
        response.headers[0].1.clone(),
        String::from_utf8(bytes.to_vec()).unwrap(),
    )
}

#[test]
fn page_shells_carry_chrome_but_no_core_data() {
    let html = page::render(Page::Overlays, "2.0.0-alpha.1");
    assert!(!html.contains("{{"), "every placeholder is substituted");
    // The first paint already carries the page's title, tab and search.
    assert!(html.contains(r#"<body data-page="installed">"#));
    assert!(html.contains(r#"<h1 id="page-title" tabindex="-1">Overlays</h1>"#));
    assert!(html.contains(r#"data-page="installed" href="/" aria-current="page""#));
    assert!(html.contains(r#"placeholder="Search overlays""#));
    assert!(html.contains("search-bar"));
    assert!(html.contains(r#"<span class="status-value">2.0.0-alpha.1</span>"#));
    // Lists, counts and the update notice arrive from core's JSON routes.
    assert!(html.contains(r#"class="empty-state results-loading" role="status""#));
    assert!(html.contains(r#"data-count="installed"></span>"#));
    assert!(html.contains(r#"data-action="show-update" hidden"#));
    assert!(html.contains(r#"id="update-notice""#));
    assert!(!html.contains(r#"class="catalog-notice""#));
    // Core injects the context at the one marker; the shell holds no secret.
    assert_eq!(html.matches(CONTEXT_MARKER).count(), 1);
    assert!(!html.contains("vosu-context\""));
    assert!(!html.contains("csrf"));

    let tools = page::render(Page::Tools, "v");
    assert!(tools.contains(r#"data-page="tools" href="/tools" aria-current="page""#));
    assert!(!tools.contains(r#"href="/" aria-current"#));
    assert!(tools.contains(r#"<div id="tools-root" class="tools-root"></div>"#));
    let overlay_settings = page::render(Page::OverlaySettings, "v");
    assert!(!overlay_settings.contains("search-bar"));
    assert!(
        overlay_settings.contains(r#"<h1 id="page-title" tabindex="-1">Overlay settings</h1>"#)
    );
    let available = page::render(Page::Available, "v");
    // The third-party disclosure never depends on a reachable catalog.
    assert!(available.contains(r#"class="catalog-notice""#));
    assert!(available.contains(r#"placeholder="Search available overlays""#));
    let hostile = page::render(Page::Overlays, "<p>{{LIST}} {{UNKNOWN</p>");
    assert!(
        hostile.contains("&lt;p&gt;{{LIST}} {{UNKNOWN&lt;/p&gt;"),
        "substituted content is escaped and never rescanned for placeholders"
    );
}

#[test]
fn settings_shell_has_every_control_and_bound_but_no_value() {
    let settings = page::render(Page::Settings, "2.0.0");
    assert!(settings.contains(r#"<p class="settings-status" hidden></p>"#));
    assert!(
        settings
            .contains(r#"<form class="settings" id="runtime-settings" aria-busy="true" inert>"#)
    );
    for binding in [
        "ENABLE_AUTOUPDATE",
        "OPEN_DASHBOARD_ON_STARTUP",
        #[cfg(target_os = "linux")]
        "START_AT_LOGIN",
        "API_V1_SUBSCRIPTION_DEBOUNCE",
        "SERVER_IP",
        "SERVER_PORT",
        "DASHBOARD_IP",
        "DASHBOARD_PORT",
        "ALLOWED_IPS",
        "DEBUG_LOG",
        "MAX_PACKAGE_MIB",
    ] {
        assert!(
            settings.contains(&format!(r#"data-id="{binding}""#)),
            "{binding}"
        );
    }
    #[cfg(target_os = "linux")]
    {
        assert!(settings.contains("Run vosu as a systemd user service when you log in."));
        assert!(settings.contains(
            r#"data-id="START_AT_LOGIN" type="checkbox" role="switch" aria-describedby="setting-START_AT_LOGIN-description">"#
        ));
    }
    #[cfg(windows)]
    assert!(!settings.contains("START_AT_LOGIN"));
    // The dashboard never offers to disable itself (A7.3); retired keys and
    // keys moved to plugin settings (D19, A7.1-OR) stay off this page.
    for key in [
        "ENABLE_DASHBOARD",
        "ENABLE_KEY_OVERLAY",
        "ENABLE_MANIA_KEY_HIT_ERRORS",
        "ENABLE_HIT_OFFSETS",
        "V2_SUBSCRIPTION_DEBOUNCE",
        "CALCULATE_PP",
        "READ_MANIA_SCROLL_SPEED",
        "POLL_RATE",
        "PRECISE_DATA_POLL_RATE",
        "SHOW_MP_COMMANDS",
        "STATIC_FOLDER_PATH",
    ] {
        assert!(!settings.contains(key), "{key}");
    }
    assert!(
        !settings.contains(" checked"),
        "values come from SettingsView"
    );
    assert!(settings.contains(
        r#"data-id="API_V1_SUBSCRIPTION_DEBOUNCE" type="number" value="" min="1" max="60000" step="any" required"#
    ));
    assert!(settings.contains(
        r#"data-id="MAX_PACKAGE_MIB" type="number" value="" min="1" max="4096" step="1" required"#
    ));
    assert!(settings.contains(
        r#"data-id="SERVER_PORT" type="number" value="" min="1" max="65535" step="1" required"#
    ));
    assert!(settings.contains(
        r#"<span class="settings-item__control settings-item__value">vosu 2.0.0</span>"#
    ));
}

#[test]
fn settings_query_selects_overlay_before_tool_as_on_the_client() {
    assert_eq!(Page::for_request("/", &[]), Some(Page::Overlays));
    assert_eq!(Page::for_request("/settings", &[]), Some(Page::Settings));
    assert_eq!(
        Page::for_request("/settings", &query(&[("overlay", "a"), ("tool", "b")])),
        Some(Page::OverlaySettings)
    );
    assert_eq!(
        Page::for_request("/settings", &query(&[("overlay", ""), ("tool", "b")])),
        Some(Page::ToolSettings)
    );
    assert_eq!(Page::for_request("/report", &[]), None);
}

#[test]
fn registrations_are_exact_dashboard_gets_for_pages_and_the_dashboard_set() {
    let routes = Routes::new("2.0.0");
    let descriptors = routes.descriptors();
    let mut paths = Vec::new();
    for (descriptor, _) in &descriptors {
        assert_eq!(descriptor.listener, ListenerRole::Dashboard);
        assert_eq!(descriptor.method, "GET");
        assert!(!descriptor.websocket);
        let RoutePath::Exact(path) = &descriptor.path else {
            panic!("only exact registrations: {descriptor:?}");
        };
        paths.push(path.as_str());
    }
    for expected in [
        "/",
        "/settings",
        "/available",
        "/tools",
        "/assets/homepage.js",
        "/assets/homepage-settings.js",
        "/assets/dashboard-status.js",
        "/assets/dashboard-pages.js",
        "/assets/update-notice.css",
        "/assets/dashboard-notices.js",
        "/assets/dashboard-notices.css",
    ] {
        assert!(paths.contains(&expected), "{expected}");
    }
    // Other owners' files are never served by the dashboard host.
    for foreign in [
        "/assets/homepage.html",
        "/assets/report.html",
        "/assets/design.html",
        "/assets/tool-bridge.js",
        "/assets/tool-bridge-v1.js",
        "/assets/tokens.css",
        "/favicon.ico",
    ] {
        assert!(!paths.contains(&foreign), "{foreign}");
        assert!(routes.response(foreign, &[]).is_none(), "{foreign}");
    }
    assert_eq!(paths.len(), 4 + 18);

    let (content_type, script) = body(&routes, "/assets/homepage.js", &[]);
    assert_eq!(content_type, "text/javascript; charset=utf-8");
    assert_eq!(
        script.as_bytes(),
        include_bytes!("../../../assets/server/homepage.js")
    );
    let (content_type, _) = body(&routes, "/assets/homepage.css", &[]);
    assert_eq!(content_type, "text/css; charset=utf-8");
    let (content_type, html) = body(&routes, "/settings", &[("tool", "fixture")]);
    assert_eq!(content_type, "text/html; charset=utf-8");
    assert!(html.contains(r#"<h1 id="page-title" tabindex="-1">Tool settings</h1>"#));
}
