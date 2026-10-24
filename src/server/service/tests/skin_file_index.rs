fn skin_report_value(selected: &BeatmapFiles, index: &files::SkinFileIndex) -> Value {
    serde_json::to_value(skin_metadata(selected, index)).unwrap()
}

fn skin_index_delegate(root: &Path, files: BeatmapFiles) -> ProductionRouteDelegate {
    let counters = Arc::new(ReloadableCounterService::production(root.join("static"), root.join("settings"), root.join("cache")));
    let (_, events) = tokio::sync::watch::channel(0);
    let state = Arc::new(NoState { events });
    let relays = Arc::new(WebSocketRelaySink::new());
    let processor = Arc::new(CommandProcessor::new(Arc::new(CommandProviders), relays.clone()));
    let unused = Arc::new(UnusedServices);
    ProductionRouteDelegate::new(ProductionServices::with_native_report_probe(
        PageService, unused.clone(), counters, Arc::new(Current { files: Some(files), bytes: None }),
        unused.clone(), unused.clone(), unused,
        WebSocketEndpoints {
            v1: WebSocketHub::new(websocket::Schema::V1, state.clone()),
            sc: WebSocketHub::new(websocket::Schema::StreamCompanion, state.clone()),
            v2: WebSocketHub::new(websocket::Schema::V2, state.clone()),
            precise: WebSocketHub::new(websocket::Schema::V2Precise, state.clone()),
            commands: WebSocketHub::new(websocket::Schema::V1, state.clone()),
            subscriptions: V2SubscriptionHub::new(state, std::time::Duration::ZERO), processor, relays,
        },
    ))
}

#[tokio::test]
async fn skin_report_entries_are_additive_conditional_and_invalidate_without_skin_switch() {
    let root = tempfile::tempdir().unwrap();
    let skin = root.path().join("skin"); std::fs::create_dir(&skin).unwrap();
    std::fs::write(skin.join("skin.ini"), b"[General]").unwrap();
    let delegate = skin_index_delegate(root.path(), BeatmapFiles {
        skin_root: Some(skin.clone()), skin_client: Some("stable"), skin_name: Some("fixture".into()), ready: true, ..Default::default()
    });
    let request = || Request::builder().uri("/files/skin/").body(Body::empty()).unwrap();
    let first = delegate.file(Endpoint::SkinFile, request()).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()[header::CACHE_CONTROL], "no-cache");
    let etag = first.headers()[header::ETAG].clone();
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(first.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["files"], true);
    assert_eq!(body["entries"][0]["path"], "skin.ini");
    assert_eq!(body["entries"][0]["size"], 9);
    assert!(body["entries"][0]["modified"].is_u64());
    assert!(body["entries"][0].get("hash").is_none());
    assert_eq!(body["truncated"], false);
    let cached = delegate.file(Endpoint::SkinFile, Request::builder().uri("/files/skin/").header(header::IF_NONE_MATCH, &etag).body(Body::empty()).unwrap()).await;
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
    assert!(axum::body::to_bytes(cached.into_body(), usize::MAX).await.unwrap().is_empty());
    std::fs::write(skin.join("skin.ini"), b"[General]\nName: changed").unwrap();
    let changed = delegate.file(Endpoint::SkinFile, Request::builder().uri("/files/skin/").header(header::IF_NONE_MATCH, &etag).body(Body::empty()).unwrap()).await;
    assert_eq!(changed.status(), StatusCode::OK);
    assert_ne!(changed.headers()[header::ETAG], etag);
}

#[tokio::test]
async fn skin_and_beatmap_files_have_validators_conditional_precedence_head_and_unchanged_ranges() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("mania")).unwrap();
    std::fs::write(root.path().join("mania/note.png"), b"abcdef").unwrap();
    std::fs::write(root.path().join("audio.mp3"), b"abcdef").unwrap();
    let delegate = skin_index_delegate(root.path(), BeatmapFiles {
        skin_root: Some(root.path().to_owned()), songs_root: Some(root.path().to_owned()),
        beatmap_file: Some("audio.mp3".into()), audio_file: Some("audio.mp3".into()), background_file: Some("mania/note.png".into()),
        ready: true, ..Default::default()
    });
    for (endpoint, path) in [(Endpoint::SkinFile, "/files/skin/MANIA/NOTE.PNG"),
        (Endpoint::BeatmapFile, "/files/beatmap/file"), (Endpoint::BeatmapAudio, "/files/beatmap/audio"),
        (Endpoint::BeatmapBackground, "/files/beatmap/background"), (Endpoint::BeatmapPath, "/files/beatmap/audio.mp3")] {
        assert_eq!(crate::server::routes::classify(&Method::HEAD, path), Some(endpoint));
        let make = || Request::builder().uri(path);
        let first = delegate.file(endpoint, make().body(Body::empty()).unwrap()).await;
        assert_eq!(first.status(), StatusCode::OK, "{path}");
        let etag = first.headers()[header::ETAG].clone();
        let modified = first.headers()[header::LAST_MODIFIED].clone();
        assert!(!etag.to_str().unwrap().starts_with("W/"));
        assert_eq!(first.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(axum::body::to_bytes(first.into_body(), usize::MAX).await.unwrap(), b"abcdef".as_slice());
        for header in [header::IF_NONE_MATCH, header::IF_MODIFIED_SINCE] {
            let value = if header == header::IF_NONE_MATCH { &etag } else { &modified };
            let response = delegate.file(endpoint, make().header(header, value).body(Body::empty()).unwrap()).await;
            assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
            assert_eq!(response.headers()[header::ETAG], etag);
            assert!(axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap().is_empty());
        }
        for condition in ["*".to_owned(), format!("\"unrelated\", W/{}", etag.to_str().unwrap())] {
            let cached_head = delegate.file(endpoint, make().method(Method::HEAD).header(header::IF_NONE_MATCH, condition).body(Body::empty()).unwrap()).await;
            assert_eq!(cached_head.status(), StatusCode::NOT_MODIFIED);
            assert!(axum::body::to_bytes(cached_head.into_body(), usize::MAX).await.unwrap().is_empty());
        }
        let unmatched = delegate.file(endpoint, make().header(header::IF_NONE_MATCH, "\"different\"").header(header::IF_MODIFIED_SINCE, &modified).body(Body::empty()).unwrap()).await;
        assert_eq!(unmatched.status(), StatusCode::OK, "ETag condition takes precedence");
        let head = delegate.file(endpoint, make().method(Method::HEAD).header(header::RANGE, "bytes=1-2").body(Body::empty()).unwrap()).await;
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "6");
        assert_eq!(head.headers()[header::ETAG], etag);
        assert!(axum::body::to_bytes(head.into_body(), usize::MAX).await.unwrap().is_empty());
        let partial = delegate.file(endpoint, make().header(header::RANGE, "bytes=1-2").body(Body::empty()).unwrap()).await;
        assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(axum::body::to_bytes(partial.into_body(), usize::MAX).await.unwrap(), b"bc".as_slice());
    }
}

#[tokio::test]
async fn lazer_file_etag_is_the_store_hash() {
    let root = tempfile::tempdir().unwrap();
    let hash = "b".repeat(64); let relative = format!("b/bb/{hash}");
    std::fs::create_dir_all(root.path().join("b/bb")).unwrap();
    std::fs::write(root.path().join(&relative), b"data").unwrap();
    let delegate = skin_index_delegate(root.path(), BeatmapFiles { lazer: true, ready: true,
        songs_root: Some(root.path().to_owned()), beatmap_file: Some(relative.clone().into()),
        skin: Some(Arc::new(crate::model::memory::CurrentSkin { name: "skin".into(), base: None, builtin: None, files: [("skin.ini".into(), relative)].into() })),
        ..Default::default()
    });
    let report = delegate.file(Endpoint::SkinFile, Request::builder().uri("/files/skin/").body(Body::empty()).unwrap()).await;
    let report: Value = serde_json::from_slice(&axum::body::to_bytes(report.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(report["entries"][0]["path"], "skin.ini");
    assert_eq!(report["entries"][0]["hash"], hash);
    assert_eq!(report["entries"][0]["size"], 4);
    assert!(report["entries"][0].get("modified").is_none());
    for (endpoint, path) in [(Endpoint::SkinFile, "/files/skin/skin.ini"), (Endpoint::BeatmapFile, "/files/beatmap/file")] {
        let response = delegate.file(endpoint, Request::builder().uri(path).body(Body::empty()).unwrap()).await;
        assert_eq!(response.headers()[header::ETAG], format!("\"{hash}\""));
    }
}
