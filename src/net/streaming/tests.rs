use super::archive::preflight_zip_entries;
use super::*;
use std::io::Write;

#[test]
fn public_policy_rejects_special_and_embedded_ranges() {
    for value in [
        "0.0.0.0",
        "10.0.0.1",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.1.1",
        "172.16.0.1",
        "192.168.0.1",
        "192.0.2.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "::",
        "::1",
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.1",
        "fc00::1",
        "fe80::1",
        "2001:db8::1",
        "3fff::1",
        "2002::1",
        "64:ff9b:1::1",
        "64:ff9b::a00:1",
        "64:ff9b::7f00:1",
        "64:ff9b::c000:201",
        "64:ff9b:0:1::101:101",
        "64:ff9b:2::101:101",
    ] {
        assert!(!is_global_unicast(value.parse().unwrap()), "{value}");
    }
    assert!(is_global_unicast("1.1.1.1".parse().unwrap()));
    assert!(is_global_unicast("2606:4700:4700::1111".parse().unwrap()));
    assert!(is_global_unicast("64:ff9b::101:101".parse().unwrap()));
}

#[derive(Debug)]
struct FixedResolver(Vec<IpAddr>);
impl AddressResolver for FixedResolver {
    fn resolve<'a>(&'a self, _: &'a str) -> ResolverFuture<'a> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

fn test_client(addresses: Vec<IpAddr>) -> StreamingClient {
    StreamingClient::new(
        Arc::new(FixedResolver(addresses)),
        DestinationPolicy::PublicOnly,
        Deadlines::default(),
        DownloadLimits::default(),
    )
}

#[tokio::test]
async fn complete_dns_answer_is_validated_and_http_is_rejected() {
    let mixed = test_client(vec![
        "1.1.1.1".parse().unwrap(),
        "127.0.0.1".parse().unwrap(),
    ]);
    assert!(matches!(
        mixed
            .resolve_and_validate(&Url::parse("https://mixed.test/a").unwrap())
            .await,
        Err(NetworkError::Destination(_))
    ));
    let public = test_client(vec!["1.1.1.1".parse().unwrap()]);
    assert!(matches!(
        public
            .resolve_and_validate(&Url::parse("http://public.test/a").unwrap())
            .await,
        Err(NetworkError::Scheme)
    ));
}

#[derive(Debug)]
struct SequenceResolver(tokio::sync::Mutex<std::collections::VecDeque<Vec<IpAddr>>>);
impl AddressResolver for SequenceResolver {
    fn resolve<'a>(&'a self, _: &'a str) -> ResolverFuture<'a> {
        Box::pin(async move { Ok(self.0.lock().await.pop_front().unwrap_or_default()) })
    }
}

#[tokio::test]
async fn every_hop_gets_one_fresh_complete_dns_validation() {
    let resolver = Arc::new(SequenceResolver(tokio::sync::Mutex::new(
        [
            vec!["1.1.1.1".parse().unwrap()],
            vec!["127.0.0.1".parse().unwrap()],
        ]
        .into(),
    )));
    let client = StreamingClient::new(
        resolver,
        DestinationPolicy::PublicOnly,
        Deadlines::default(),
        DownloadLimits::default(),
    );
    let url = Url::parse("https://rebind.test/a").unwrap();
    assert_eq!(
        client.resolve_and_validate(&url).await.unwrap().2,
        vec!["1.1.1.1".parse::<IpAddr>().unwrap()]
    );
    assert!(matches!(
        client.resolve_and_validate(&url).await,
        Err(NetworkError::Destination(_))
    ));
}

#[derive(Debug)]
struct ManualTime(Arc<tokio::sync::Notify>);
impl TimeSource for ManualTime {
    fn sleep(&self, _: Duration) -> SleepFuture<'_> {
        let notify = self.0.clone();
        Box::pin(async move { notify.notified().await })
    }
}

#[derive(Debug)]
struct NeverResolver;
impl AddressResolver for NeverResolver {
    fn resolve<'a>(&'a self, _: &'a str) -> ResolverFuture<'a> {
        Box::pin(core::future::pending())
    }
}

#[tokio::test]
async fn injected_time_source_controls_absolute_deadline() {
    let notify = Arc::new(tokio::sync::Notify::new());
    let client = StreamingClient::new(
        Arc::new(NeverResolver),
        DestinationPolicy::PublicOnly,
        Deadlines::default(),
        DownloadLimits::default(),
    )
    .with_time_source(Arc::new(ManualTime(notify.clone())));
    notify.notify_one();
    assert!(matches!(
        client
            .request_bytes("https://deadline.test/a", "test", &CancellationToken::new())
            .await,
        Err(NetworkError::Timeout)
    ));
}

#[tokio::test]
async fn cancellation_interrupts_dns_resolution() {
    let client = StreamingClient::new(
        Arc::new(NeverResolver),
        DestinationPolicy::PublicOnly,
        Deadlines::default(),
        DownloadLimits::default(),
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        client
            .request_bytes("https://cancel.test/a", "test", &cancel)
            .await,
        Err(NetworkError::Cancelled)
    ));
}

#[test]
fn redirects_are_relative_bounded_and_never_downgrade() {
    let current = Url::parse("https://example.test/a/b").unwrap();
    let relative = reqwest::header::HeaderValue::from_static("../c");
    assert_eq!(
        redirect_target(&current, Some(&relative), 0, 5)
            .unwrap()
            .as_str(),
        "https://example.test/c"
    );
    let downgrade = reqwest::header::HeaderValue::from_static("http://example.test/c");
    assert!(matches!(
        redirect_target(&current, Some(&downgrade), 0, 5),
        Err(NetworkError::Downgrade)
    ));
    assert!(matches!(
        redirect_target(&current, Some(&relative), 5, 5),
        Err(NetworkError::RedirectLimit)
    ));
}

#[cfg(feature = "test-harness")]
async fn harness_fixture() -> (StreamingClient, String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let socket = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request = vec![0; 2048];
                let Ok(length) = stream.read(&mut request).await else {
                    return;
                };
                let first = String::from_utf8_lossy(&request[..length]);
                let path = first.split_whitespace().nth(1).unwrap_or("/");
                if path == "/headers" {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    return;
                }
                let response = match path {
                    "/ok" => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    "/json" => {
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
                    }
                    "/status" => {
                        "HTTP/1.1 500 Error\r\nContent-Length: 100\r\nConnection: keep-alive\r\n\r\n"
                    }
                    "/large" => {
                        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n12345"
                    }
                    "/loop" => "HTTP/1.1 302 Found\r\nLocation: /loop\r\nContent-Length: 0\r\n\r\n",
                    "/idle" | "/slow" => {
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nx"
                    }
                    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
                };
                let _ = stream.write_all(response.as_bytes()).await;
                if path == "/idle" || path == "/slow" || path == "/status" {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            });
        }
    });
    let config = TestNetworkConfig::new(
        Arc::new(FixedResolver(vec![socket.ip()])),
        Arc::from([String::from("fixture.invalid")]),
        Arc::from([socket]),
    )
    .unwrap();
    let (resolver, policy) = config.into_parts();
    let client = StreamingClient::new(
        resolver,
        policy,
        Deadlines {
            connect: Duration::from_secs(1),
            idle: Duration::from_millis(20),
            total: Duration::from_secs(5),
        },
        DownloadLimits {
            max_bytes: 4,
            max_redirects: 2,
        },
    );
    (
        client,
        format!("http://fixture.invalid:{}", socket.port()),
        task,
    )
}

#[cfg(feature = "test-harness")]
#[tokio::test]
#[ignore]
async fn proxy_bypass_child() {
    if std::env::var_os("VOSU_PROXY_BYPASS_CHILD").is_none() {
        return;
    }
    let (client, base, server) = harness_fixture().await;
    let response = client
        .request_bytes(&format!("{base}/ok"), "test", &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(response.body, "ok");
    server.abort();
}

#[cfg(feature = "test-harness")]
#[tokio::test]
async fn no_proxy_ignores_process_proxy_configuration() {
    let executable = std::env::current_exe().unwrap();
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new(executable)
            .arg("--ignored")
            .arg("--exact")
            .arg("net::streaming::tests::proxy_bypass_child")
            .env("VOSU_PROXY_BYPASS_CHILD", "1")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .status()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(status.success());
}

#[cfg(feature = "test-harness")]
#[tokio::test]
async fn harness_allowlist_status_timeouts_redirects_json_and_caps() {
    let (client, base, server) = harness_fixture().await;
    assert_eq!(
        client
            .request_bytes(&format!("{base}/ok"), "test", &CancellationToken::new())
            .await
            .unwrap()
            .body,
        "ok"
    );
    let status = client
        .request_bytes(&format!("{base}/status"), "test", &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(status.status, 500);
    assert!(status.body.is_empty());
    assert!(
        client
            .request_json::<serde_json::Value>(
                &format!("{base}/json"),
                "test",
                &CancellationToken::new()
            )
            .await
            .unwrap()
            .is_object()
    );
    assert!(matches!(
        client
            .request_bytes(&format!("{base}/large"), "test", &CancellationToken::new())
            .await,
        Err(NetworkError::TooLarge(4))
    ));
    assert!(matches!(
        client
            .request_bytes(&format!("{base}/idle"), "test", &CancellationToken::new())
            .await,
        Err(NetworkError::Timeout)
    ));
    assert!(matches!(
        client
            .request_bytes(
                &format!("{base}/headers"),
                "test",
                &CancellationToken::new()
            )
            .await,
        Err(NetworkError::Timeout)
    ));
    let loop_error = client
        .request_bytes(&format!("{base}/loop"), "test", &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(loop_error, NetworkError::RedirectLimit),
        "{loop_error:?}"
    );
    let denied = Url::parse(&base.replace("fixture.invalid", "other.invalid")).unwrap();
    assert!(matches!(
        client.resolve_and_validate(&denied).await,
        Err(NetworkError::Destination(_))
    ));
    server.abort();
}
#[cfg(feature = "test-harness")]
#[test]
fn harness_config_rejects_nonreserved_hosts_and_nonloopback_sockets() {
    assert!(
        TestNetworkConfig::new(
            Arc::new(FixedResolver(vec!["127.0.0.1".parse().unwrap()])),
            Arc::from([String::from("production.example")]),
            Arc::from(["127.0.0.1:1".parse().unwrap()]),
        )
        .is_err()
    );
    assert!(
        TestNetworkConfig::new(
            Arc::new(FixedResolver(vec!["1.1.1.1".parse().unwrap()])),
            Arc::from([String::from("fixture.invalid")]),
            Arc::from(["1.1.1.1:443".parse().unwrap()]),
        )
        .is_err()
    );
}

#[cfg(feature = "test-harness")]
#[tokio::test]
async fn cancelled_and_oversized_downloads_remove_parts_and_preserve_destination() {
    let (client, base, server) = harness_fixture().await;
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("file");
    std::fs::write(&target, b"old").unwrap();
    let oversized = client
        .download_atomic(
            &format!("{base}/large"),
            "test",
            &target,
            &CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(&oversized, Err(NetworkError::TooLarge(4))),
        "unexpected oversized-download result: {oversized:?}"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    let cancel = CancellationToken::new();
    let later = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        later.cancel();
    });
    assert!(matches!(
        client
            .download_atomic(&format!("{base}/slow"), "test", &target, &cancel)
            .await,
        Err(NetworkError::Cancelled)
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    client
        .download_atomic(
            &format!("{base}/ok"),
            "test",
            &target,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"ok");
    assert!(!std::fs::read_dir(root.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part")
    }));
    server.abort();
}

fn make_zip(entries: &[(&str, &[u8], Option<u32>)]) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    {
        let mut writer = zip::ZipWriter::new(file.reopen().unwrap());
        for (name, body, mode) in entries {
            let options = zip::write::SimpleFileOptions::default();
            if mode.is_some_and(|mode| mode & 0o170000 == 0o120000) {
                writer
                    .add_symlink(*name, String::from_utf8_lossy(body), options)
                    .unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(body).unwrap();
            }
        }
        writer.finish().unwrap();
    }
    file
}

#[test]
fn zip64_entry_cap_is_checked_before_archive_construction() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut bytes = vec![0u8; 56];
    bytes[..4].copy_from_slice(b"PK\x06\x06");
    bytes[4..12].copy_from_slice(&44u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&100_001u64.to_le_bytes());
    let mut locator = [0u8; 20];
    locator[..4].copy_from_slice(b"PK\x06\x07");
    locator[8..16].copy_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&locator);
    let mut eocd = [0u8; 22];
    eocd[..4].copy_from_slice(b"PK\x05\x06");
    eocd[8..12].copy_from_slice(&[0xff; 4]);
    bytes.extend_from_slice(&eocd);
    std::fs::write(file.path(), bytes).unwrap();
    let mut input = file.reopen().unwrap();
    assert!(matches!(
        preflight_zip_entries(&mut input, 100_000, &CancellationToken::new()),
        Err(NetworkError::UnsafeZip(message)) if message == "entry limit exceeded"
    ));
}

#[test]
fn preflight_checks_fallback_eocd_candidates_and_accepts_classic_65535() {
    let zip = make_zip(&[("a", b"x", None)]);
    let mut bytes = std::fs::read(zip.path()).unwrap();
    let real = bytes
        .windows(4)
        .rposition(|window| window == b"PK\x05\x06")
        .unwrap();
    bytes[real + 8..real + 12].copy_from_slice(&100u16.to_le_bytes().repeat(2));
    let mut fake = [0u8; 22];
    fake[..4].copy_from_slice(b"PK\x05\x06");
    fake[8..12].copy_from_slice(&1u16.to_le_bytes().repeat(2));
    // Keep the rejected fallback outside the conventional EOCD tail.
    bytes.resize(bytes.len() + 70_000, 0);
    bytes.extend_from_slice(&fake);
    std::fs::write(zip.path(), bytes).unwrap();
    assert!(matches!(
        preflight_zip_entries(&mut zip.reopen().unwrap(), 10, &CancellationToken::new()),
        Err(NetworkError::UnsafeZip(message)) if message == "entry limit exceeded"
    ));

    let classic = tempfile::NamedTempFile::new().unwrap();
    let mut eocd = [0u8; 22];
    eocd[..4].copy_from_slice(b"PK\x05\x06");
    eocd[8..12].copy_from_slice(&[0xff; 4]);
    std::fs::write(classic.path(), eocd).unwrap();
    preflight_zip_entries(
        &mut classic.reopen().unwrap(),
        100_000,
        &CancellationToken::new(),
    )
    .unwrap();
}

#[test]
fn zip_rejects_traversal_symlink_and_caps() {
    let root = tempfile::tempdir().unwrap();
    for zip in [
        make_zip(&[("../escape", b"x", None)]),
        make_zip(&[("/absolute", b"x", None)]),
        make_zip(&[("link", b"target", Some(0o120777))]),
    ] {
        let result = extract_zip_atomic(
            zip.path(),
            &root.path().join("out"),
            ZipLimits::default(),
            &CancellationToken::new(),
        );
        assert!(
            result.is_err(),
            "accepted unsafe entry {}",
            zip::ZipArchive::new(zip.reopen().unwrap())
                .unwrap()
                .by_index(0)
                .unwrap()
                .name()
        );
    }
    let zip = make_zip(&[("a", b"123", None), ("b", b"4", None)]);
    assert!(
        extract_zip_atomic(
            zip.path(),
            &root.path().join("out"),
            ZipLimits {
                max_entries: 1,
                max_expanded_bytes: 100
            },
            &CancellationToken::new()
        )
        .is_err()
    );
    assert!(
        extract_zip_atomic(
            zip.path(),
            &root.path().join("out"),
            ZipLimits {
                max_entries: 2,
                max_expanded_bytes: 3
            },
            &CancellationToken::new()
        )
        .is_err()
    );
    let deep = make_zip(&[("a/b/c/file", b"", None)]);
    assert!(matches!(
        extract_zip_atomic(
            deep.path(),
            &root.path().join("deep"),
            ZipLimits { max_entries: 3, max_expanded_bytes: 100 },
            &CancellationToken::new(),
        ),
        Err(NetworkError::UnsafeZip(message)) if message == "extracted path limit exceeded"
    ));
}

#[test]
fn zip_install_is_atomic_and_preserves_old_on_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("counter");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("old"), b"old").unwrap();
    let zip = make_zip(&[("nested/new", b"new", None)]);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        extract_zip_atomic(zip.path(), &target, ZipLimits::default(), &cancel),
        Err(NetworkError::Cancelled)
    ));
    assert_eq!(std::fs::read(target.join("old")).unwrap(), b"old");
    extract_zip_atomic(
        zip.path(),
        &target,
        ZipLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(std::fs::read(target.join("nested/new")).unwrap(), b"new");
    assert!(!target.join("old").exists());
}
