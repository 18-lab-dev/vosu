#![cfg(target_os = "linux")]

use std::{
    fs,
    net::{Ipv4Addr, SocketAddrV4, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

struct ChildGuard(Option<Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn available_port() -> u16 {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn config(port: u16, static_root: &Path) -> String {
    format!(
        "ENABLE_AUTOUPDATE=false\nOPEN_DASHBOARD_ON_STARTUP=false\nDEBUG_LOG=false\nPOLL_RATE=150\nPRECISE_DATA_POLL_RATE=10\nSERVER_IP=127.0.0.1\nSERVER_PORT={port}\nSTATIC_FOLDER_PATH={}\nALLOWED_IPS=127.0.0.1,localhost,absolute\n",
        static_root.display(),
    )
}

async fn wait_get(client: &reqwest::Client, url: &str) -> reqwest::Response {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match client.get(url).send().await {
            Ok(response) => return response,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("daemon did not become reachable at {url}: {error}"),
        }
    }
}

async fn websocket_connect(port: u16, path: &str) -> (tokio::net::TcpStream, Vec<u8>) {
    let mut stream = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\n\
                 Host: localhost\r\n\
                 Connection: Upgrade\r\n\
                 Upgrade: websocket\r\n\
                 Sec-WebSocket-Version: 13\r\n\
                 Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    let mut response = Vec::new();
    let header_end = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(offset) = response.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "daemon closed during WebSocket upgrade");
            response.extend_from_slice(&chunk[..read]);
        }
    })
    .await
    .expect("WebSocket upgrade timed out");
    assert!(
        response[..header_end].starts_with(b"HTTP/1.1 101"),
        "unexpected WebSocket upgrade response: {}",
        String::from_utf8_lossy(&response[..header_end])
    );
    (stream, response.split_off(header_end))
}

fn masked_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() < 126);
    let mask = [0x12, 0x34, 0x56, 0x78];
    let mut frame = Vec::with_capacity(payload.len() + 6);
    frame.extend_from_slice(&[0x80 | opcode, 0x80 | payload.len() as u8]);
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % mask.len()]),
    );
    frame
}

async fn read_server_frame(
    stream: &mut tokio::net::TcpStream,
    buffered: &mut Vec<u8>,
) -> (u8, Vec<u8>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while buffered.len() < 2 {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "daemon closed before WebSocket frame");
            buffered.extend_from_slice(&chunk[..read]);
        }
        assert_eq!(buffered[1] & 0x80, 0, "server frames must not be masked");
        let short_len = (buffered[1] & 0x7f) as usize;
        let (header_len, payload_len) = match short_len {
            126 => {
                while buffered.len() < 4 {
                    let mut chunk = [0_u8; 1024];
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(read, 0, "daemon closed inside WebSocket header");
                    buffered.extend_from_slice(&chunk[..read]);
                }
                (4, u16::from_be_bytes([buffered[2], buffered[3]]) as usize)
            }
            127 => panic!("smoke frame unexpectedly exceeds 64 KiB"),
            len => (2, len),
        };
        while buffered.len() < header_len + payload_len {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "daemon closed inside WebSocket payload");
            buffered.extend_from_slice(&chunk[..read]);
        }
        let frame: Vec<_> = buffered.drain(..header_len + payload_len).collect();
        (frame[0] & 0x0f, frame[header_len..].to_vec())
    })
    .await
    .expect("server WebSocket frame timed out")
}
fn assert_no_update_artifacts(root: &Path) {
    if !root.exists() {
        return;
    }
    let suspicious: Vec<PathBuf> = fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            name.contains("update-helper-")
                || name.contains("install-record")
                || name.ends_with(".part")
                || name.starts_with("vosu_old-")
        })
        .collect();
    assert!(
        suspicious.is_empty(),
        "updater artifacts remained: {suspicious:?}"
    );
}

#[tokio::test]
async fn production_binary_smoke_uses_public_only_policy_and_cleans_up() {
    let roots = TempDir::new().expect("temporary daemon roots");
    let config_home = roots.path().join("config");
    let data_home = roots.path().join("data");
    let cache_home = roots.path().join("cache");
    let static_root = roots.path().join("counter-static");
    let counter = static_root.join("seeded-counter");
    fs::create_dir_all(&counter).unwrap();
    let seeded_index = "<!doctype html><title>Seeded</title>";
    fs::write(counter.join("index.html"), seeded_index).unwrap();
    let seeded_script = b"window.seeded = true;\n";
    fs::write(counter.join("app.js"), seeded_script).unwrap();
    fs::write(
        counter.join("metadata.txt"),
        "Name: Seeded Counter\nAuthor: CI\nVersion: 1.0\n640x480\n",
    )
    .unwrap();
    fs::write(
        counter.join("settings.json"),
        r#"[{"uniqueID":"title","type":"text","title":"Title","value":"seeded"}]"#,
    )
    .unwrap();
    fs::create_dir_all(
        static_root
            .join("directory-index-counter")
            .join("index.html"),
    )
    .unwrap();
    #[cfg(unix)]
    let (alias_index, alias_asset) = {
        let outside_counter = roots.path().join("outside-static-root");
        fs::create_dir_all(&outside_counter).unwrap();
        let index = b"<!doctype html><title>External Alias</title>";
        fs::write(outside_counter.join("index.html"), index).unwrap();
        let asset = b"window.externalAlias = true;\n";
        fs::write(outside_counter.join("alias.js"), asset).unwrap();
        fs::write(
            outside_counter.join("metadata.txt"),
            "Name: External Alias Counter\nAuthor: CI\nVersion: 1.0\n640x480\n",
        )
        .unwrap();
        fs::write(
            outside_counter.join("settings.json"),
            r#"[{"uniqueID":"title","type":"text","title":"Title","value":"alias"}]"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside_counter, static_root.join("external-alias")).unwrap();
        (index.as_slice(), asset.as_slice())
    };

    // The old `tosu` root and `tosu.env` are one-time migration inputs.
    let old_config_dir = config_home.join("tosu");
    fs::create_dir_all(&old_config_dir).unwrap();
    let port = available_port();
    fs::write(old_config_dir.join("tosu.env"), config(port, &static_root)).unwrap();
    let old_settings_dir = old_config_dir.join("settings");
    fs::create_dir_all(&old_settings_dir).unwrap();
    fs::write(
        old_settings_dir.join("seeded-counter.values.json"),
        r#"{"title":"persisted seeded"}"#,
    )
    .unwrap();

    let binary = std::env::var_os("VOSU_RELEASE_SMOKE_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_vosu").into());
    let child = Command::new(&binary)
        .arg("--update=false")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_CACHE_HOME", &cache_home)
        .env("HOME", roots.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn actual current vosu executable");
    let mut child = ChildGuard(Some(child));

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let dashboard = wait_get(&client, &format!("{base}/")).await;
    assert_eq!(dashboard.status(), 200);
    let dashboard = dashboard.text().await.unwrap();
    assert!(dashboard.contains("vosu"));
    let css = client
        .get(format!("{base}/assets/homepage.min.css"))
        .send()
        .await
        .unwrap();
    assert_eq!(css.status(), 200);
    assert!(!css.bytes().await.unwrap().is_empty());

    let seeded_counter = client
        .get(format!("{base}/seeded-counter/"))
        .send()
        .await
        .unwrap();
    assert_eq!(seeded_counter.status(), 200);
    assert_eq!(
        seeded_counter.headers()[reqwest::header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
    assert_eq!(
        seeded_counter.text().await.unwrap(),
        format!(r#"{seeded_index}<script>window.COUNTER_PATH="seeded-counter";</script>"#)
    );

    let no_redirect_client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let seeded_counter_redirect = no_redirect_client
        .get(format!("{base}/seeded-counter?foo=bar"))
        .send()
        .await
        .unwrap();
    assert!(seeded_counter_redirect.status().is_redirection());
    assert_eq!(
        seeded_counter_redirect.headers()[reqwest::header::LOCATION],
        "/seeded-counter/?foo=bar"
    );

    let missing_counter = no_redirect_client
        .get(format!("{base}/definitely-missing"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing_counter.status(), 404);

    let seeded_script_response = no_redirect_client
        .get(format!("{base}/seeded-counter/app%2Ejs"))
        .send()
        .await
        .unwrap();
    assert_eq!(seeded_script_response.status(), 200);
    assert_eq!(
        seeded_script_response.bytes().await.unwrap().as_ref(),
        seeded_script.as_slice()
    );
    #[cfg(unix)]
    {
        let alias = no_redirect_client
            .get(format!("{base}/external-alias/"))
            .send()
            .await
            .unwrap();
        assert_eq!(alias.status(), 200);
        let mut expected_alias = alias_index.to_vec();
        expected_alias
            .extend_from_slice(br#"<script>window.COUNTER_PATH="external-alias";</script>"#);
        assert_eq!(
            alias.bytes().await.unwrap().as_ref(),
            expected_alias.as_slice()
        );

        let alias_asset_response = no_redirect_client
            .get(format!("{base}/external-alias/alias.js"))
            .send()
            .await
            .unwrap();
        assert_eq!(alias_asset_response.status(), 200);
        assert_eq!(
            alias_asset_response.bytes().await.unwrap().as_ref(),
            alias_asset
        );
    }

    let directory_index = no_redirect_client
        .get(format!("{base}/directory-index-counter/"))
        .send()
        .await
        .unwrap();
    assert_eq!(directory_index.status(), 404);

    let not_ready = client.get(format!("{base}/json/v2")).send().await.unwrap();
    assert_eq!(not_ready.status(), 500);
    assert_eq!(
        not_ready.text().await.unwrap(),
        r#"{"error":"osu is not ready/running"}"#
    );

    let (mut subscription, mut buffered) = websocket_connect(port, "/websocket/v2/subscribe").await;
    subscription
        .write_all(&masked_frame(0x1, br#"["/play/score"]"#))
        .await
        .unwrap();
    let (opcode, payload) = read_server_frame(&mut subscription, &mut buffered).await;
    assert_eq!(opcode, 0x1, "initial subscription message must be text");
    let initial: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    let initial = initial
        .as_array()
        .expect("initial subscription frame must be an array");
    assert_eq!(initial.len(), 3);
    assert_eq!(initial[0], 0);
    assert!(initial[1].as_u64().is_some(), "revision must be an integer");
    assert_eq!(initial[2], serde_json::json!([]));

    subscription
        .write_all(&masked_frame(0x8, &1000_u16.to_be_bytes()))
        .await
        .unwrap();
    let (opcode, payload) = read_server_frame(&mut subscription, &mut buffered).await;
    assert_eq!(opcode, 0x8, "daemon must complete the WebSocket close");
    assert!(
        payload.is_empty() || payload.starts_with(&1000_u16.to_be_bytes()),
        "unexpected close payload: {payload:?}"
    );

    let catalog = client
        .get(format!("{base}/api/counters/search/seeded"))
        .send()
        .await
        .unwrap();
    assert_eq!(catalog.status(), 200);
    let body = catalog.text().await.unwrap();
    assert!(body.contains("Seeded Counter"), "counter response: {body}");
    let seeded_settings = client
        .get(format!("{base}/api/counters/settings/seeded-counter"))
        .send()
        .await
        .unwrap();
    assert_eq!(seeded_settings.status(), 200);
    let seeded_settings: serde_json::Value = seeded_settings.json().await.unwrap();
    assert_eq!(
        seeded_settings,
        serde_json::json!({
            "settings": [{
                "uniqueID": "title",
                "type": "text",
                "title": "Title",
                "description": "",
                "value": "seeded"
            }],
            "values": {
                "title": "persisted seeded"
            }
        })
    );
    #[cfg(unix)]
    {
        let alias_catalog = client
            .get(format!("{base}/api/counters/search/alias"))
            .send()
            .await
            .unwrap();
        assert_eq!(alias_catalog.status(), 200);
        let alias_body = alias_catalog.text().await.unwrap();
        assert!(
            alias_body.contains("External Alias Counter"),
            "counter response: {alias_body}"
        );
        assert!(
            alias_body.contains("/external-alias/"),
            "counter alias missing from response: {alias_body}"
        );
        let alias_settings = client
            .get(format!("{base}/api/counters/settings/external-alias"))
            .send()
            .await
            .unwrap();
        assert_eq!(alias_settings.status(), 200);
        assert!(
            alias_settings
                .text()
                .await
                .unwrap()
                .contains(r#""uniqueID":"title""#),
            "symlinked overlay settings were not returned"
        );
    }

    let rejected = client.get(format!(
        "{base}/api/counters/download/http%3A%2F%2F127.0.0.1%3A9%2Fcounter.zip?name=blocked&update=false"
    )).send().await.unwrap();
    assert_eq!(rejected.status(), 200);
    let rejected_body = rejected.text().await.unwrap();
    assert!(
        rejected_body.contains("error"),
        "loopback download was not rejected: {rejected_body}"
    );
    assert!(!static_root.join("blocked").exists());

    let report = client
        .get(format!("{base}/api/generateReport"))
        .send()
        .await
        .unwrap();
    assert_eq!(report.status(), 200);
    let disposition = report
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(disposition.contains("vosu-report-") && disposition.ends_with(".html\""));
    assert!(report.text().await.unwrap().contains("vosu"));

    let next_port = available_port();
    fs::write(
        config_home.join("vosu").join("vosu.env"),
        config(next_port, &static_root),
    )
    .unwrap();
    let next_base = format!("http://127.0.0.1:{next_port}");
    let rebound = wait_get(&client, &format!("{next_base}/json")).await;
    assert_eq!(rebound.status(), 500);
    assert_eq!(
        rebound.text().await.unwrap(),
        r#"{"error":"osu is not ready/running"}"#
    );

    let deleted = client
        .get(format!("{next_base}/api/counters/delete/seeded-counter"))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 200);
    assert!(!counter.exists());

    assert!(
        config_home.join("vosu").join("vosu.env").is_file(),
        "migrated config missing"
    );
    assert!(!old_config_dir.exists(), "legacy config was not removed");

    let pid = child.0.as_ref().unwrap().id() as libc::pid_t;
    let signal_result = unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(signal_result, 0, "send graceful SIGTERM");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
            assert!(status.success(), "daemon exited unsuccessfully: {status}");
            child.0.take();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon failed to drain after SIGTERM"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert_no_update_artifacts(&cache_home.join("vosu"));
}
