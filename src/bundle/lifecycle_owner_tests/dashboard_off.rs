//! A7.3 dashboard off at composition: `vosu.dashboard` keeps its `BUNDLED`
//! slot, validated and listed `inactive` with reason `dashboard_disabled`,
//! but it is never created and mounts no dashboard route.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_off_lists_the_dashboard_inactive_and_never_creates_or_mounts_it() {
    tokio::time::timeout(WAIT, async {
        let root = tempfile::tempdir().unwrap();
        let owner = TestDaemon::new(root.path(), &BUNDLE).await;
        let management = owner.load_all(false).unwrap();
        let instance = BUNDLE.instance(BUNDLE.dashboard).unwrap();
        assert!(
            owner.plugins().surface_requester(instance).is_err(),
            "the dashboard instance is never created"
        );

        let response = management
            .handle(
                axum::http::Request::builder()
                    .uri("/api/plugins")
                    .body(axum::body::Body::empty())
                    .unwrap(),
                axum::body::Bytes::new(),
            )
            .await;
        let list: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap(),
        )
        .unwrap();
        let plugins = list["plugins"].as_array().unwrap();
        // The registry still equals `BUNDLED` exactly, in order.
        assert_eq!(plugins.len(), BUNDLED.len());
        let ids: Vec<_> = plugins
            .iter()
            .map(|plugin| plugin["id"].as_str().unwrap())
            .collect();
        assert_eq!(&ids[..BUNDLE.required.len()], BUNDLE.required);
        let dashboard = plugins
            .iter()
            .find(|plugin| plugin["id"] == BUNDLE.dashboard)
            .unwrap();
        assert_eq!(dashboard["state"], "inactive");
        assert_eq!(dashboard["reason"], "dashboard_disabled");
        assert_eq!(dashboard["trust"], "bundled");
        assert_eq!(dashboard["generation"], 0);
        assert!(
            plugins
                .iter()
                .filter(|plugin| plugin["id"] != BUNDLE.dashboard)
                .all(|plugin| plugin["state"] == "running"),
            "{list}"
        );

        // Mounting the dashboard host needs its instance; off skips it, and
        // every other route group still registers.
        assert!(owner.start_adapters(true).await.is_err());
        let mut adapters = owner.start_adapters(false).await.unwrap();
        adapters.stop_hubs().await;
        adapters.stop_adapters().await.unwrap();
        owner.shutdown().await.unwrap();
        assert!(
            owner
                .output()
                .contains("Dashboard is off: vosu.dashboard is inactive (dashboard_disabled)")
        );
    })
    .await
    .unwrap();
}
