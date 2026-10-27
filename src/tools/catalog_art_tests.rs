use super::*;

const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><rect width="8" height="8"/></svg>"#;

#[test]
fn content_decides_the_type_and_the_extension_must_agree() {
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    let webp = b"RIFF\x10\0\0\0WEBPVP8 ";
    let jpeg = b"\xff\xd8\xff\xe0\0\x10JFIF";
    for (body, kind) in [
        (SVG, ArtKind::Svg),
        (&png[..], ArtKind::Png),
        (&webp[..], ArtKind::Webp),
        (&jpeg[..], ArtKind::Jpeg),
    ] {
        assert_eq!(sniff(body), Some(kind));
    }
    // Prologue forms real SVG files start with.
    for prologue in [
        "\u{feff}<?xml version=\"1.0\"?>\n<!-- art -->\n",
        "<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">",
        "  \n",
    ] {
        let text = [prologue.as_bytes(), SVG].concat();
        assert_eq!(sniff(&text), Some(ArtKind::Svg), "{prologue}");
    }
    for body in [
        &b"<html><svg></svg></html>"[..],
        b"<svgx>",
        b"<!DOCTYPE svg [<!ENTITY a \"b\">]><svg/>",
        b"<!-- unterminated <svg>",
        b"GIF89a",
        b"\xff\xfe<\0s\0v\0g\0",
        b"",
    ] {
        assert_eq!(sniff(body), None, "{}", String::from_utf8_lossy(body));
    }
    assert_eq!(
        classify("https://example.com/a/icon.svg", SVG),
        Ok(ArtKind::Svg)
    );
    assert_eq!(
        classify("https://example.com/a/ICON.SVG?x=1", SVG),
        Ok(ArtKind::Svg)
    );
    assert_eq!(
        classify("https://example.com/download", SVG),
        Ok(ArtKind::Svg)
    );
    assert_eq!(
        classify("https://example.com/a.jpeg", jpeg),
        Ok(ArtKind::Jpeg)
    );
    assert!(classify("https://example.com/a.png", SVG).is_err());
    assert!(classify("https://example.com/a.gif", SVG).is_err());
    assert!(classify("https://example.com/a.svg", b"<html></html>").is_err());
}

#[test]
fn cache_is_bounded_and_dropped_with_the_catalog_revision() {
    let mut cache = ArtCache::default();
    let first = [1; 32];
    assert!(cache.get(first, "a").is_none());
    let big = Bytes::from(vec![0; 1024 * 1024]);
    for index in 0..20 {
        cache.insert(first, format!("{index}"), ArtKind::Png, big.clone());
    }
    assert!(cache.bytes <= CACHE_BYTES);
    assert_eq!(cache.len(), CACHE_BYTES / big.len());
    // Least recently used goes first.
    assert!(cache.get(first, "0").is_none());
    assert!(cache.get(first, "19").is_some());
    for index in 0..300 {
        cache.insert(
            first,
            format!("small-{index}"),
            ArtKind::Svg,
            Bytes::from_static(b"x"),
        );
    }
    assert_eq!(cache.len(), CACHE_ENTRIES);
    // Another revision starts empty and late inserts for the old one are dropped.
    assert!(cache.get([2; 32], "small-299").is_none());
    assert_eq!((cache.len(), cache.bytes), (0, 0));
    cache.insert(first, "late".into(), ArtKind::Svg, Bytes::from_static(b"x"));
    assert_eq!(cache.len(), 0);
}

#[cfg(feature = "test-harness")]
mod routes {
    use super::super::super::catalog::{Overlay, Package, Tool};
    use super::*;
    use crate::net::{Deadlines, DownloadLimits, StreamingClient, TestNetworkConfig};
    use axum::http::{Request, header};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const HOST: &str = "art.invalid";

    struct Resolver;
    impl crate::net::AddressResolver for Resolver {
        fn resolve<'a>(&'a self, _: &'a str) -> crate::net::ResolverFuture<'a> {
            Box::pin(async { Ok(vec![std::net::IpAddr::from([127, 0, 0, 1])]) })
        }
    }

    /// A stand-in release host: SVG served as `application/octet-stream`
    /// behind a redirect, like GitHub release downloads.
    async fn upstream(hits: Arc<AtomicUsize>) -> std::net::SocketAddr {
        use axum::{
            Router,
            response::{IntoResponse, Redirect},
            routing::get,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket = listener.local_addr().unwrap();
        let octet = |body: Vec<u8>| {
            ([(header::CONTENT_TYPE, "application/octet-stream")], body).into_response()
        };
        let app =
            Router::new()
                .route(
                    "/download/icon.svg",
                    get(move || async move { Redirect::to("/storage/abc").into_response() }),
                )
                .route(
                    "/storage/abc",
                    get(move || async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        octet(SVG.to_vec())
                    }),
                )
                .route(
                    "/page.svg",
                    get(move || async move { octet(b"<html></html>".to_vec()) }),
                )
                .route(
                    "/named.png",
                    get(move || async move { octet(SVG.to_vec()) }),
                )
                .route(
                    "/big.png",
                    get(move || async move {
                        let mut body = b"\x89PNG\r\n\x1a\n".to_vec();
                        body.resize(MAX_ART_BYTES as usize + 1, 0);
                        octet(body)
                    }),
                )
                .route(
                    "/private.svg",
                    get(move || async move {
                        Redirect::to(&format!("http://127.0.0.1:{}/storage/abc", socket.port()))
                            .into_response()
                    }),
                )
                .route(
                    "/internal.svg",
                    get(move || async move {
                        Redirect::to("https://10.0.0.1/icon.svg").into_response()
                    }),
                );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        socket
    }

    fn tool(id: &str, icon: String, banner: String) -> Tool {
        Tool {
            id: id.into(),
            source: "18-lab-dev/vosu-tools".into(),
            name: id.into(),
            version: "1.0.0".into(),
            description: "fixture".into(),
            author: "18-LAB".into(),
            homepage: None,
            icon_url: Some(icon),
            banner_url: Some(banner),
            protocol: 1,
            interfaces: vec!["ui".into()],
            requires: Vec::new(),
            replaces: Vec::new(),
            packages: BTreeMap::from([(
                "any".into(),
                Package {
                    url: "https://example.com/tool.zip".into(),
                    sha256: "00".repeat(32),
                    size: 1,
                },
            )]),
        }
    }

    async fn host(root: &Path, socket: std::net::SocketAddr) -> Arc<ToolHost> {
        let host = ToolHost::new(
            root.join("config"),
            root.join("data"),
            root.join("cache"),
            ExtraRoots { dev: Vec::new() },
            "http://127.0.0.1:25150".into(),
            "http://127.0.0.1:25151".into(),
            Arc::new(|| json!({})),
        )
        .await
        .unwrap();
        let network =
            TestNetworkConfig::new(Arc::new(Resolver), vec![HOST.into()], vec![socket]).unwrap();
        host.art.lock().client = Some(StreamingClient::for_test_network(
            network,
            Deadlines::default(),
            DownloadLimits {
                max_bytes: MAX_ART_BYTES,
                max_redirects: 5,
            },
        ));
        host
    }

    async fn get(host: &Arc<ToolHost>, url: &str) -> (StatusCode, axum::http::HeaderMap, Bytes) {
        let query: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("url", url)
            .finish();
        let request = Request::builder()
            .uri(format!("/api/tools/catalog/art?{query}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let response = host.dashboard(request, Bytes::new()).await;
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, body)
    }

    fn assert_inert(headers: &axum::http::HeaderMap) {
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], ART_CSP);
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers["cross-origin-resource-policy"], "same-origin");
        assert_eq!(headers[header::CONTENT_DISPOSITION], "inline");
        assert!(headers.get(header::SET_COOKIE).is_none());
    }

    #[tokio::test]
    async fn listed_art_is_proxied_typed_cached_and_inert() {
        let hits = Arc::new(AtomicUsize::new(0));
        let socket = upstream(hits.clone()).await;
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path(), socket).await;
        let base = format!("http://{HOST}:{}", socket.port());
        let icon = format!("{base}/download/icon.svg");
        let preview = format!("{base}/page.svg");
        *host.catalog.lock() = Catalog {
            catalog_version: 1,
            published: "2026-10-26T00:00:00Z".into(),
            tools: vec![tool("vosulay", icon.clone(), format!("{base}/big.png"))],
            overlays: vec![Overlay {
                id: "hud".into(),
                name: "HUD".into(),
                version: "1.0.0".into(),
                description: String::new(),
                author: "18-LAB".into(),
                homepage: None,
                icon_url: None,
                banner_url: None,
                resolution: None,
                preview: Some(preview.clone()),
                requires_tools: Vec::new(),
                package: Package {
                    url: "https://example.com/hud.zip".into(),
                    sha256: "00".repeat(32),
                    size: 1,
                },
                source: "18-lab-dev/vosulay".into(),
            }],
            shadowed: vec![json!({"kind":"tool","icon_url":format!("{base}/named.png"),
                "banner_url":format!("{base}/private.svg"),"preview":format!("{base}/internal.svg")})],
            ..Catalog::default()
        };

        // Listed: fetched through the redirect, typed by content, inert.
        let (status, headers, body) = get(&host, &icon).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "image/svg+xml");
        assert_eq!(body, SVG);
        assert_inert(&headers);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // Cached for this catalog revision.
        assert_eq!(get(&host, &icon).await.0, StatusCode::OK);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // A new catalog revision fetches again.
        host.catalog.lock().published = "2026-10-27T00:00:00Z".into();
        assert_eq!(get(&host, &icon).await.0, StatusCode::OK);
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // Not listed: never fetched, whatever the host.
        for url in [
            format!("{base}/storage/abc"),
            "https://example.com/icon.svg".into(),
            String::new(),
        ] {
            let (status, headers, _) = get(&host, &url).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{url}");
            assert_inert(&headers);
        }
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let bare = Request::builder()
            .uri("/api/tools/catalog/art")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            host.dashboard(bare, Bytes::new()).await.status(),
            StatusCode::NOT_FOUND
        );

        // Listed but refused: wrong type, extension mismatch, oversize, and
        // redirects to a private or non-catalog destination.
        for (url, reason) in [
            (preview.as_str(), "not an SVG"),
            (&format!("{base}/named.png"), "does not match"),
            (&format!("{base}/big.png"), "exceeds 1048576 bytes"),
            (&format!("{base}/private.svg"), "not permitted: 127.0.0.1"),
            (&format!("{base}/internal.svg"), "not permitted: 10.0.0.1"),
        ] {
            let (status, headers, body) = get(&host, url).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{url}");
            assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
            assert_inert(&headers);
            let text = String::from_utf8_lossy(&body).to_lowercase();
            assert!(text.contains(&reason.to_lowercase()), "{url}: {text}");
        }
        assert_eq!(host.art.lock().len(), 1);

        // Clearing the sources empties the allowlist.
        *host.catalog.lock() = Catalog::default();
        assert_eq!(get(&host, &icon).await.0, StatusCode::NOT_FOUND);
        host.shutdown().await;
    }
}
