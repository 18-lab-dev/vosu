#![expect(
    clippy::disallowed_methods,
    reason = "test fixtures write scratch files and sleep"
)]

use std::{
    fs,
    net::{Ipv4Addr, SocketAddrV4, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use vosu_proptest::bounded;

use tempfile::TempDir;

/// The test-harness adapter capture endpoint (GOLDEN_CONTRACT.md §5), which
/// `build.rs` keeps out of every release build.
const HARNESS_PATH: &str = "/__vosu_test/v1/adapter-capture?schema=v2";
const HARNESS_HEADER: &str = "x-vosu-test-capture";
const HARNESS_CAPABILITY_ENV: &str = "VOSU_TEST_ADAPTER_CAPTURE";
const HARNESS_CAPABILITY: &str = "3e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c3d2e1f";

fn assert_no_test_harness_endpoint(executable: &[u8]) {
    for marker in ["/__vosu_test/", HARNESS_HEADER, HARNESS_CAPABILITY_ENV] {
        assert!(
            !executable
                .windows(marker.len())
                .any(|window| window == marker.as_bytes()),
            "release executable contains {marker}"
        );
    }
}

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

fn legacy_config(port: u16, static_root: &Path) -> String {
    format!(
        "ENABLE_AUTOUPDATE=false\nOPEN_DASHBOARD_ON_STARTUP=false\nDEBUG_LOG=false\nPOLL_RATE=150\nPRECISE_DATA_POLL_RATE=10\nSERVER_IP=127.0.0.1\nSERVER_PORT={port}\nDASHBOARD_PORT=0\nSTATIC_FOLDER_PATH={}\nALLOWED_IPS=127.0.0.1,localhost,absolute\n",
        static_root.display(),
    )
}

fn toml_config(port: u16, static_root: &Path) -> String {
    let static_root = toml::Value::String(static_root.to_string_lossy().into_owned());
    format!(
        "ENABLE_AUTOUPDATE = false\nOPEN_DASHBOARD_ON_STARTUP = false\nDEBUG_LOG = false\nPOLL_RATE = 150\nPRECISE_DATA_POLL_RATE = 10\nSERVER_IP = \"127.0.0.1\"\nSERVER_PORT = {port}\nDASHBOARD_PORT = 0\nSTATIC_FOLDER_PATH = {static_root}\nALLOWED_IPS = \"127.0.0.1,localhost,absolute\"\n"
    )
}

async fn wait_get(client: &reqwest::Client, url: &str) -> reqwest::Response {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match bounded!(client.get(url).send()) {
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
    // SAFETY: `libc::kill` with a valid pid and signal number has no memory
    // safety preconditions; `child` is this test's own spawned process.
    let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    assert_eq!(result, 0, "send release daemon SIGINT");
}

#[cfg(windows)]
fn request_graceful_shutdown(child: &Child) {
    // SAFETY: `GenerateConsoleCtrlEvent` with a valid process group id has no
    // memory safety preconditions; `child` is this test's own spawned
    // process, started in its own process group via `CREATE_NEW_PROCESS_GROUP`.
    let result = unsafe {
        windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(
            windows_sys::Win32::System::Console::CTRL_BREAK_EVENT,
            child.id(),
        )
    };
    assert_ne!(result, 0, "send release daemon CTRL_BREAK_EVENT");
}

#[cfg(target_os = "linux")]
fn assert_packaged_linux_cli_preflight(binary: &Path) {
    let help = Command::new(binary)
        .arg("--help")
        .output()
        .expect("run packaged Linux release --help preflight");
    assert!(
        help.status.success(),
        "packaged Linux release --help failed with {}\nstdout:\n{}\nstderr:\n{}",
        help.status,
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr),
    );
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("-s, --setup"),
        "packaged Linux release --help omitted -s, --setup\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr),
    );

    for probe in [
        "--vosu-internal-probe-backend=ebpf",
        "--vosu-internal-probe-backend=cn-proc",
    ] {
        let output = Command::new(binary)
            .arg(probe)
            .output()
            .unwrap_or_else(|error| panic!("run packaged Linux release probe {probe}: {error}"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            !combined.contains("this vosu build does not include Linux process-event backends"),
            "packaged Linux release probe {probe} reached a featureless backend path with {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status,
        );
    }
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

    #[cfg(target_os = "linux")]
    assert_packaged_linux_cli_preflight(&binary);
    assert_no_test_harness_endpoint(&fs::read(&binary).expect("read release executable"));

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
    let config_path = config_dir.join("vosu.toml");
    // A valid current TOML file is authoritative over both legacy env inputs.
    fs::write(&config_path, toml_config(first_port, &static_root)).unwrap();
    fs::write(config_dir.join("vosu.env"), legacy_config(1, &static_root)).unwrap();
    fs::write(config_dir.join("tosu.env"), legacy_config(2, &static_root)).unwrap();

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
        // A release build has no harness endpoint to mount, even when offered
        // a well-formed capability.
        .env(HARNESS_CAPABILITY_ENV, HARNESS_CAPABILITY)
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
    let dashboard = bounded!(wait_get(&client, &format!("{base}/")));
    assert_eq!(dashboard.status(), 200);
    let management = dashboard.url().origin().ascii_serialization();
    let dashboard = bounded!(dashboard.text()).unwrap();
    assert!(dashboard.contains("vosu"));
    let csrf = dashboard
        .split("\"csrf\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let canonical: toml::Table =
        toml::from_str(&fs::read_to_string(&config_path).expect("read canonical packaged config"))
            .expect("packaged daemon canonicalizes current TOML");
    assert_eq!(
        canonical.get("SERVER_PORT").and_then(toml::Value::as_float),
        Some(f64::from(first_port))
    );
    assert!(!config_dir.join("vosu.env").exists());
    assert!(!config_dir.join("tosu.env").exists());
    let asset = bounded!(client.get(format!("{base}/assets/homepage.css")).send()).unwrap();
    assert_eq!(asset.status(), 200);
    assert!(!bounded!(asset.bytes()).unwrap().is_empty());

    let mut replies = Vec::new();
    for (path, capability) in [
        ("/__vosu_test/v1/unknown-sibling?schema=v2", None),
        (HARNESS_PATH, Some(HARNESS_CAPABILITY)),
        (HARNESS_PATH, None),
    ] {
        let mut request = client.get(format!("{base}{path}"));
        if let Some(capability) = capability {
            request = request.header(HARNESS_HEADER, capability);
        }
        let response = bounded!(request.send()).unwrap();
        replies.push((response.status(), bounded!(response.bytes()).unwrap()));
    }
    assert_eq!(replies[0].0, 404);
    assert!(
        replies.iter().all(|reply| reply == &replies[0]),
        "release build answered the harness endpoint: {replies:?}"
    );

    let state = bounded!(client.get(format!("{base}/json")).send()).unwrap();
    let state_status = state.status();
    assert!(
        state_status == 200 || state_status == 500,
        "unexpected state response: {state_status}"
    );
    if state_status == 500 {
        assert_eq!(
            bounded!(state.text()).unwrap(),
            r#"{"error":"osu is not ready/running"}"#
        );
    }

    let counters = bounded!(
        client
            .get(format!("{base}/api/counters/search/seeded"))
            .send()
    )
    .unwrap();
    assert_eq!(counters.status(), 200);
    assert!(
        bounded!(counters.text())
            .unwrap()
            .contains("Seeded Counter")
    );

    let rejected = bounded!(client.post(format!(
        "{management}/api/counters/download/https%3A%2F%2F127.0.0.1%3A9%2Fcounter.zip?name=blocked&update=false"
    )).header(reqwest::header::ORIGIN, &management).header("X-Vosu-CSRF", csrf).send()).unwrap();
    assert_eq!(rejected.status(), 200);
    assert!(bounded!(rejected.text()).unwrap().contains("error"));
    assert!(!static_root.join("blocked").exists());

    let report = bounded!(
        client
            .post(format!("{management}/api/generateReport"))
            .header(reqwest::header::ORIGIN, &management)
            .header("X-Vosu-CSRF", csrf)
            .send()
    )
    .unwrap();
    assert_eq!(report.status(), 200);
    let disposition = report
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(disposition.contains("vosu-report-") && disposition.contains(".html"));
    assert!(bounded!(report.text()).unwrap().contains("vosu"));

    let second_port = available_port();
    fs::write(
        &config_path,
        format!("SERVER_PORT = {second_port}\nBROKEN = [\n"),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let last_good_client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let last_good = bounded!(last_good_client.get(format!("{base}/")).send())
        .expect("malformed TOML must retain the packaged listener");
    assert_eq!(last_good.status(), 200);

    fs::write(&config_path, toml_config(second_port, &static_root)).unwrap();
    let second_base = format!("http://127.0.0.1:{second_port}");
    let rebound = bounded!(wait_get(&client, &format!("{second_base}/")));
    assert_eq!(rebound.status(), 200);

    let saved = bounded!(
        client
            .post(format!("{management}/api/settingsSave"))
            .header(reqwest::header::ORIGIN, &management)
            .header("X-Vosu-CSRF", csrf)
            .json(&serde_json::json!({"SHOW_MP_COMMANDS": true}))
            .send()
    )
    .expect("save packaged uppercase JSON settings");
    assert_eq!(saved.status(), 200);
    assert_eq!(
        bounded!(saved.json::<serde_json::Value>()).unwrap(),
        serde_json::json!({"status":"updated"})
    );
    let persisted: toml::Table =
        toml::from_str(&fs::read_to_string(&config_path).expect("read packaged settingsSave TOML"))
            .expect("packaged settingsSave persists typed TOML");
    assert_eq!(
        persisted
            .get("SHOW_MP_COMMANDS")
            .and_then(toml::Value::as_bool),
        Some(true)
    );

    let deleted = bounded!(
        client
            .post(format!("{management}/api/counters/delete/seeded-counter"))
            .header(reqwest::header::ORIGIN, &management)
            .header("X-Vosu-CSRF", csrf)
            .send()
    )
    .unwrap();
    assert_eq!(deleted.status(), 200);
    assert!(!seeded.exists());
    assert!(config_path.is_file());
    assert!(!config_dir.join("vosu.env").exists());
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
