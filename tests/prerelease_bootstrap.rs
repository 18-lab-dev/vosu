//! Real executable coverage for the one-time alpha bootstrap.
#![cfg(target_os = "linux")]
#![expect(
    clippy::disallowed_methods,
    reason = "test fixtures write scratch files"
)]

use std::{
    fs,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
use vosu_proptest::bounded;

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = vosu_proptest::wait::child(&mut self.0);
    }
}

fn spawn(root: &Path) -> Daemon {
    Daemon(
        Command::new(env!("CARGO_BIN_EXE_vosu"))
            .arg("--update=false")
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_RUNTIME_DIR", root.join("runtime"))
            .env("HOME", root)
            .env("VOSU_PROCESS_EVENTS", "poll")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

async fn reachable(port: u16) {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(response) = client
                .get(format!("http://127.0.0.1:{port}/api/dashboard"))
                .send()
                .await
            {
                assert!(response.status().is_success());
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("alpha daemon startup deadline");
}

#[tokio::test]
async fn prerelease_daemon_startup_restart_preserves_normal_settings() {
    assert!(env!("CARGO_PKG_VERSION").contains("-alpha."));
    let root = tempfile::tempdir().unwrap();
    let normal = root.path().join("config/vosu");
    fs::create_dir_all(&normal).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let source = format!(
        "SERVER_PORT = {port}\nDASHBOARD_PORT = 0\nPOLL_RATE = 842.0\nENABLE_AUTOUPDATE = false\nOPEN_DASHBOARD_ON_STARTUP = false\nSTATIC_FOLDER_PATH = 'normal-write-sentinel'\nUNKNOWN_SENTINEL = 'retain bytes'\n"
    );
    fs::write(normal.join("vosu.toml"), &source).unwrap();
    fs::write(normal.join("vosu.env"), "POLL_RATE=421\n").unwrap();
    fs::write(normal.join("broker.json"), "private broker sentinel").unwrap();
    let daemon = spawn(root.path());
    bounded!(reachable(port));
    let alpha = normal.join("prerelease/vosu.toml");
    let imported = fs::read_to_string(&alpha).unwrap();
    assert!(imported.contains("POLL_RATE = 842.0"));
    assert!(imported.contains("STATIC_FOLDER_PATH = \"./static\""));
    assert_eq!(
        fs::read_to_string(normal.join("vosu.toml")).unwrap(),
        source
    );
    assert_eq!(
        fs::read_to_string(normal.join("vosu.env")).unwrap(),
        "POLL_RATE=421\n"
    );
    assert_eq!(
        fs::read_to_string(normal.join("broker.json")).unwrap(),
        "private broker sentinel"
    );
    assert!(root.path().join("data/vosu/prerelease/static").is_dir());
    assert!(!root.path().join("data/vosu/static").exists());
    drop(daemon);

    fs::write(normal.join("vosu.toml"), "POLL_RATE = [broken on restart").unwrap();
    let restarted = spawn(root.path());
    bounded!(reachable(port));
    assert_eq!(fs::read_to_string(alpha).unwrap(), imported);
    assert_eq!(
        fs::read_to_string(normal.join("vosu.toml")).unwrap(),
        "POLL_RATE = [broken on restart"
    );
    drop(restarted);
}
