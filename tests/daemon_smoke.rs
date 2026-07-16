#![cfg(target_os = "linux")]

use std::{
    fs,
    net::{Ipv4Addr, SocketAddrV4, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use tempfile::TempDir;

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
    fs::write(
        counter.join("index.html"),
        "<!doctype html><title>Seeded</title>",
    )
    .unwrap();
    fs::write(
        counter.join("metadata.txt"),
        "Name: Seeded Counter\nAuthor: CI\nVersion: 1.0\n640x480\n",
    )
    .unwrap();

    // The old `tosu` root and `tosu.env` are one-time migration inputs.
    let old_config_dir = config_home.join("tosu");
    fs::create_dir_all(&old_config_dir).unwrap();
    let port = available_port();
    fs::write(old_config_dir.join("tosu.env"), config(port, &static_root)).unwrap();

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

    let not_ready = client.get(format!("{base}/json/v2")).send().await.unwrap();
    assert_eq!(not_ready.status(), 500);
    assert_eq!(
        not_ready.text().await.unwrap(),
        r#"{"error":"osu is not ready/running"}"#
    );

    let catalog = client
        .get(format!("{base}/api/counters/search/seeded"))
        .send()
        .await
        .unwrap();
    assert_eq!(catalog.status(), 200);
    let body = catalog.text().await.unwrap();
    assert!(body.contains("Seeded Counter"), "counter response: {body}");

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
