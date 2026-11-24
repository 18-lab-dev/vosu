
fn skin_index_delegate(root: &Path, files: BeatmapFiles) -> ProductionRouteDelegate {
    let counters = Arc::new(ReloadableCounterService::production(root.join("static"), root.join("settings"), root.join("cache")));
    let (_, events) = tokio::sync::watch::channel(0);
    let state = Arc::new(NoState { events });
    let relays = Arc::new(WebSocketRelaySink::new());
    let processor = Arc::new(CommandProcessor::new(Arc::new(CommandProviders), relays.clone()));
    let unused = Arc::new(UnusedServices);
    ProductionRouteDelegate::new(ProductionServices::with_native_report_probe(
        PageService, unused.clone(), counters, Arc::new(Current { files: Some(files) }),
        unused.clone(), unused.clone(), unused,
        WebSocketEndpoints {
            v1: WebSocketHub::new(websocket::Schema::V1, state.clone()),
            sc: WebSocketHub::new(websocket::Schema::StreamCompanion, state.clone()),
            v2: WebSocketHub::new(websocket::Schema::V2, state.clone()),
            precise: WebSocketHub::new(websocket::Schema::V2Precise, state.clone()),
            commands: WebSocketHub::new(websocket::Schema::V1, state.clone()),
            processor, relays,
        },
    ))
}

#[tokio::test]
async fn legacy_skin_report_is_removed_without_metadata_or_upgrade() {
    let root = tempfile::tempdir().unwrap();
    let delegate = skin_index_delegate(root.path(), BeatmapFiles::default());
    let request = Request::builder().uri("/files/skin/").header(header::UPGRADE, "websocket").body(Body::empty()).unwrap();
    let response = bounded!(delegate.dispatch(Endpoint::Removed(super::super::routes::RemovedFeature::LegacySkinIndex), request, Bytes::new()));
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(!response.headers().contains_key(header::UPGRADE));
    assert_eq!(bounded!(axum::body::to_bytes(response.into_body(), 1024)).unwrap(), "Not Found");
}


#[tokio::test]
async fn removed_routes_attribute_only_installed_explicit_identity_or_local_provenance() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("static/counter")).unwrap();
    std::fs::write(root.path().join("static/counter/index.html"), b"overlay").unwrap();
    let delegate = skin_index_delegate(root.path(), BeatmapFiles::default());
    for (uri, referer, origin, expected) in [
        ("/websocket/v2/subscribe?l=counter", None, None, Some("counter")),
        ("/websocket/v2/subscribe?l=uninstalled", None, None, None),
        ("/websocket/v2/subscribe", Some("http://localhost:24050/counter/index.html"), None, Some("counter")),
        ("/websocket/v2/subscribe", Some("http://foreign.test/counter/"), None, None),
        ("/websocket/v2/subscribe", Some("https://localhost:24050/counter/"), None, None),
        ("/websocket/v2/subscribe", Some("http://localhost:24050/unknown/"), None, None),
        ("/websocket/v2/subscribe", None, Some("http://localhost:24050"), None),
    ] {
        let mut request = Request::builder().uri(uri).header(header::HOST, "localhost:24050");
        if let Some(value) = referer { request = request.header(header::REFERER, value); }
        if let Some(value) = origin { request = request.header(header::ORIGIN, value); }
        let before: u64 = delegate.removed_notices().iter().filter(|notice| notice.overlay.as_deref() == expected).map(|notice| notice.attempts).sum();
        let response = bounded!(delegate.dispatch(Endpoint::Removed(super::super::routes::RemovedFeature::SelectedV2Feed), request.body(Body::empty()).unwrap(), Bytes::new()));
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let after: u64 = delegate.removed_notices().iter().filter(|notice| notice.overlay.as_deref() == expected).map(|notice| notice.attempts).sum();
        assert_eq!(after, before + 1, "{uri}");
    }
    std::fs::remove_dir_all(root.path().join("static/counter")).unwrap();
    assert!(delegate.removed_notices().iter().all(|notice| notice.overlay.is_none()));
    bounded!(delegate.services.counters.shutdown());
    bounded!(delegate.services.websocket.processor.shutdown());
    bounded!(delegate.services.websocket.relays.shutdown());
}
