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
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match client.get(url).send().await {
            Ok(response) => return response,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("release daemon did not become reachable at {url}: {error}"),
        }
    }
}

#[cfg(unix)]
fn configure_process(_command: &mut Command) {}

#[cfg(windows)]
fn configure_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn request_graceful_shutdown(child: &Child) {
    let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    assert_eq!(result, 0, "send release daemon SIGINT");
}

#[cfg(windows)]
fn request_graceful_shutdown(child: &Child) {
    let result = unsafe {
        windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(
            windows_sys::Win32::System::Console::CTRL_BREAK_EVENT,
            child.id(),
        )
    };
    assert_ne!(result, 0, "send release daemon CTRL_BREAK_EVENT");
}

fn assert_no_update_artifacts(root: &Path) {
    if !root.exists() {
        return;
    }
    let mut stack = vec![root.to_path_buf()];
    let mut suspicious = Vec::<PathBuf>::new();
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.contains(".vosu-update-helper-")
                || name.contains("install-record")
                || name.ends_with(".part")
                || name.starts_with(".vosu-backup-")
            {
                suspicious.push(path);
            }
        }
    }
    assert!(
        suspicious.is_empty(),
        "release left updater artifacts: {suspicious:?}"
    );
}

#[tokio::test]
#[ignore = "requires a packaged release binary"]
async fn packaged_release_obeys_production_policy_and_shuts_down_cleanly() {
    let binary = std::env::var_os("VOSU_RELEASE_SMOKE_BINARY")
        .expect("VOSU_RELEASE_SMOKE_BINARY must name the unpacked native release executable");
    let binary = fs::canonicalize(binary).expect("canonical release executable path");
    assert!(
        binary.is_file(),
        "release smoke path is not a file: {}",
        binary.display()
    );

    let roots = TempDir::new().expect("temporary release roots");
    let static_root = roots.path().join("static");
    let seeded = static_root.join("seeded-counter");
    fs::create_dir_all(&seeded).unwrap();
    fs::write(
        seeded.join("index.html"),
        "<!doctype html><title>Seeded</title>",
    )
    .unwrap();
    fs::write(
        seeded.join("metadata.txt"),
        "Name: Seeded Counter\nAuthor: CI\nVersion: 1.0\n640x480\n",
    )
    .unwrap();

    #[cfg(unix)]
    let config_dir = roots.path().join("config/vosu");
    #[cfg(windows)]
    let config_dir = binary
        .parent()
        .expect("release binary parent")
        .to_path_buf();
    fs::create_dir_all(&config_dir).unwrap();
    let first_port = available_port();
    // The former config filename is a one-time migration input for the packaged daemon.
    fs::write(
        config_dir.join("tosu.env"),
        config(first_port, &static_root),
    )
    .unwrap();

    let mut command = Command::new(&binary);
    command
        .arg("--update=false")
        .env("XDG_CONFIG_HOME", roots.path().join("config"))
        .env("XDG_DATA_HOME", roots.path().join("data"))
        .env("XDG_CACHE_HOME", roots.path().join("cache"))
        .env("HOME", roots.path())
        // Avoid the Windows archive/OneDrive guards mistaking a CI unpack root
        // for the user's configured TEMP or OneDrive tree.
        .env("TEMP", roots.path().join("runtime-temp-sentinel"))
        .env("OneDrive", roots.path().join("onedrive-sentinel"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process(&mut command);
    let mut child = ChildGuard(Some(
        command.spawn().expect("spawn unpacked release executable"),
    ));

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{first_port}");
    let dashboard = wait_get(&client, &format!("{base}/")).await;
    assert_eq!(dashboard.status(), 200);
    assert!(dashboard.text().await.unwrap().contains("vosu"));
    let asset = client
        .get(format!("{base}/assets/homepage.min.css"))
        .send()
        .await
        .unwrap();
    assert_eq!(asset.status(), 200);
    assert!(!asset.bytes().await.unwrap().is_empty());

    let state = client.get(format!("{base}/json")).send().await.unwrap();
    let state_status = state.status();
    assert!(
        state_status == 200 || state_status == 500,
        "unexpected state response: {state_status}"
    );
    if state_status == 500 {
        assert_eq!(
            state.text().await.unwrap(),
            r#"{"error":"osu is not ready/running"}"#
        );
    }

    let counters = client
        .get(format!("{base}/api/counters/search/seeded"))
        .send()
        .await
        .unwrap();
    assert_eq!(counters.status(), 200);
    assert!(counters.text().await.unwrap().contains("Seeded Counter"));

    let rejected = client.get(format!(
        "{base}/api/counters/download/https%3A%2F%2F127.0.0.1%3A9%2Fcounter.zip?name=blocked&update=false"
    )).send().await.unwrap();
    assert_eq!(rejected.status(), 200);
    assert!(rejected.text().await.unwrap().contains("error"));
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
    assert!(disposition.contains("vosu-report-") && disposition.contains(".html"));
    assert!(report.text().await.unwrap().contains("vosu"));

    let second_port = available_port();
    fs::write(
        config_dir.join("vosu.env"),
        config(second_port, &static_root),
    )
    .unwrap();
    let second_base = format!("http://127.0.0.1:{second_port}");
    let rebound = wait_get(&client, &format!("{second_base}/")).await;
    assert_eq!(rebound.status(), 200);

    let deleted = client
        .get(format!("{second_base}/api/counters/delete/seeded-counter"))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 200);
    assert!(!seeded.exists());
    assert!(config_dir.join("vosu.env").is_file());
    assert!(!config_dir.join("tosu.env").exists());

    request_graceful_shutdown(child.0.as_ref().unwrap());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
            assert!(
                status.success(),
                "release daemon exited unsuccessfully: {status}"
            );
            child.0.take();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "release daemon failed to drain after native interrupt"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[cfg(unix)]
    let cache_root = roots.path().join("cache/vosu");
    #[cfg(windows)]
    let cache_root = binary.parent().unwrap().join(".cache");
    assert_no_update_artifacts(&cache_root);
}
