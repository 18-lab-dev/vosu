
    #[tokio::test]
    async fn forced_listener_drain_drops_idle_production_bridge_children() {
        use crate::server::runtime::{
            AxumListenerFactory, ListenTarget, ListenerBinder, TokioBinder,
        };
        use tokio::io::AsyncWriteExt as _;

        let directory = tempfile::tempdir().unwrap();
        let counters = Arc::new(ReloadableCounterService::production(
            directory.path().join("static"),
            directory.path().join("settings"),
            directory.path().join("cache"),
        ));
        let (_, events) = tokio::sync::watch::channel(0);
        let state = Arc::new(NoState { events });
        let v1 = WebSocketHub::new(websocket::Schema::V1, state.clone());
        let sc = WebSocketHub::new(websocket::Schema::StreamCompanion, state.clone());
        let v2 = WebSocketHub::new(websocket::Schema::V2, state.clone());
        let precise = WebSocketHub::new(websocket::Schema::V2Precise, state.clone());
        let commands = WebSocketHub::new(websocket::Schema::V1, state.clone());
        let relays = Arc::new(WebSocketRelaySink::new());
        let command_providers = Arc::new(CommandProviders);
        let processor = Arc::new(CommandProcessor::new(command_providers, relays.clone()));
        let unused = Arc::new(UnusedServices);
        let delegate = Arc::new(ProductionRouteDelegate::new(
            ProductionServices::with_native_report_probe(
                PageService,
                unused.clone(),
                counters,
                Arc::new(Current::default()),
                unused.clone(),
                unused.clone(),
                unused,
                WebSocketEndpoints {
                    v1,
                    sc,
                    v2,
                    precise,
                    commands: commands.clone(),
                    processor,
                    relays: relays.clone(),
                },
            ),
        ));
        let snapshots = Arc::new(SnapshotManager::new(Arc::new(ManagerSnapshot::not_ready(
            1,
            ApiRuntimeSettings::default(),
        ))));
        let providers = super::super::routes::RouteProviders {
            authorization: Arc::new(AllowAll),
            state: Arc::new(SnapshotStateProvider::new(snapshots)),
            calculation: Arc::new(LegacyCalculateProvider::new(Arc::new(Current::default()))),
            delegate,
        };
        let binder = TokioBinder(AxumListenerFactory::new(providers));
        let listener = bounded!(binder
            .bind(
                &ListenTarget::new("127.0.0.1", 0),
                "127.0.0.1:0".parse().unwrap(),
            ))
            .unwrap();
        assert_eq!(
            bounded!(vosu_core::desktop::existing_dashboard("127.0.0.1", listener.address().port())),
            Some(vosu_core::desktop::dashboard_url("127.0.0.1", listener.address().port())),
        );

        let mut client = bounded!(tokio::net::TcpStream::connect(listener.address()))
            .unwrap();
        bounded!(client
            .write_all(
                b"GET /websocket/commands?l=counter HTTP/1.1\r\n\
Host: localhost\r\n\
Connection: Upgrade\r\n\
Upgrade: websocket\r\n\
Sec-WebSocket-Version: 13\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            ))
            .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !response.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0_u8; 512];
                let read = client.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0);
                response.extend_from_slice(&chunk[..read]);
            }
        })
        .await
        .expect("WebSocket upgrade must complete");
        assert!(response.starts_with(b"HTTP/1.1 101"));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while commands.client_count() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        listener.begin_drain();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), listener.drained())
                .await
                .is_err()
        );
        listener.cancel_survivors();
        tokio::time::timeout(std::time::Duration::from_secs(1), listener.drained())
            .await
            .expect("forced drain must release callback and tracked transport");
        let mut byte = [0_u8; 1];
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), client.read(&mut byte))
                .await
                .expect("client must observe shutdown")
                .unwrap(),
            0,
        );
        assert_eq!(commands.client_count(), 0);
        bounded!(relays.shutdown());
    }


    #[test]
    fn production_file_adapter_roots_generic_beatmap_at_songs() {
        use std::io::Read as _;

        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("set")).unwrap();
        std::fs::write(root.path().join("set/map.osu"), b"songs-root").unwrap();
        let selected = BeatmapFiles {
            songs_root: Some(root.path().to_owned()),
            ready: true,
            ..BeatmapFiles::default()
        };

        let ResourceOutcome::File(mut target) = resolve_file_endpoint(Endpoint::BeatmapPath,
        &selected,
        "/files/beatmap/set/map.osu",
        "/files/beatmap/set/map.osu", &mut files::SkinIndexCache::default())
        .unwrap() else {
            panic!("generic beatmap path must resolve below Songs");
        };
        let mut body = String::new();
        target.file.read_to_string(&mut body).unwrap();
        assert_eq!(body, "songs-root");
    }

    #[test]
    fn production_file_adapter_rejects_current_file_traversal() {
        let root = tempfile::tempdir().unwrap();
        let selected = BeatmapFiles {
            beatmap_file: Some(PathBuf::from("../outside.osu")),
            songs_root: Some(root.path().join("Songs")),
            ready: true,
            ..BeatmapFiles::default()
        };
        assert!(matches!(
            resolve_file_endpoint(
                Endpoint::BeatmapFile,
                &selected,
                "/files/beatmap/file",
                "/files/beatmap/file",
                &mut files::SkinIndexCache::default(),
            ),
            Ok(ResourceOutcome::Missing(MissingKind::NotFound))
        ));
    }

    #[test]
    fn production_exact_file_endpoint_rejects_directories() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("selected.osu")).unwrap();
        let selected = BeatmapFiles {
            beatmap_file: Some(PathBuf::from("selected.osu")),
            songs_root: Some(root.path().to_owned()),
            ready: true,
            ..BeatmapFiles::default()
        };
        assert!(matches!(
            resolve_file_endpoint(
                Endpoint::BeatmapFile,
                &selected,
                "/files/beatmap/file",
                "/files/beatmap/file",
                &mut files::SkinIndexCache::default(),
            ),
            Ok(ResourceOutcome::Missing(MissingKind::NotFound))
        ));
    }

    #[tokio::test]
    async fn focused_provider_without_songs_root_keeps_song_routes_not_ready() {
        let selected = BeatmapFiles {
            beatmap_file: Some(PathBuf::from("set/map.osu")),
            ready: true,
            ..BeatmapFiles::default()
        };
        for (endpoint, path) in [
            (Endpoint::SongsFile, "/Songs/set/map.osu"),
            (Endpoint::BeatmapPath, "/files/beatmap/set/map.osu"),
        ] {
            let outcome = resolve_file_endpoint(endpoint, &selected, path, path, &mut files::SkinIndexCache::default()).unwrap();
            assert!(matches!(
                outcome,
                ResourceOutcome::Missing(MissingKind::NotReady)
            ));
            let response = bounded!(resource_response(outcome, None));
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            let body = bounded!(axum::body::to_bytes(response.into_body(), usize::MAX))
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({"error": "osu is not ready/running"})
            );
        }
    }

    #[test]
    fn lazer_skin_files_are_case_insensitive_confined_and_report_builtins() {
        let root = tempfile::tempdir().unwrap();
        let hash = "a".repeat(64);
        let relative = format!("a/aa/{hash}");
        std::fs::create_dir_all(root.path().join("a/aa")).unwrap();
        std::fs::write(root.path().join(&relative), b"[General]\nName: fixture").unwrap();
        let mut selected = BeatmapFiles {
            lazer: true, ready: true, skin_client: Some("lazer"),
            skin_name: Some("Fixture".into()), songs_root: Some(root.path().to_owned()),
            skin: Some(Arc::new(vosu_osu_model::memory::CurrentSkin { base: None, name: "Fixture".into(), builtin: None,
            files: [("skin.ini".into(), relative)].into(), })),
            ..BeatmapFiles::default()
        };
        for name in ["SKIN.INI", "skin%2Eini"] {
            let path = format!("/files/skin/{name}");
            let ResourceOutcome::File(file) = resolve_file_endpoint(Endpoint::SkinFile, &selected, &path, &path, &mut files::SkinIndexCache::default()).unwrap() else { panic!("skin file not served") };
            assert_eq!(file.length, 23);
        }
        for name in ["../skin.ini", "%2e%2e/skin.ini", "/skin.ini", "%2Fskin.ini", "missing.png", "a/../../skin.ini", "C:%5cskin.ini"] {
            let path = format!("/files/skin/{name}");
            assert!(matches!(resolve_file_endpoint(Endpoint::SkinFile, &selected, &path, &path, &mut files::SkinIndexCache::default()).unwrap(), ResourceOutcome::Missing(MissingKind::NotFound)), "{name}");
        }
        selected.skin = Some(Arc::new(vosu_osu_model::memory::CurrentSkin { base: Some("argon".into()), name: "Argon".into(), builtin: Some("argon".into()), files: Default::default() }));
        assert!(matches!(resolve_file_endpoint(Endpoint::SkinFile, &selected, "/files/skin/skin.ini", "/files/skin/skin.ini", &mut files::SkinIndexCache::default()).unwrap(), ResourceOutcome::Missing(MissingKind::NotFound)));
    }
