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

type NormalInventory = std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>>;

fn normal_inventory(root: &Path) -> NormalInventory {
    fn visit(root: &Path, path: &Path, files: &mut NormalInventory) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == "prerelease" {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                files.insert(entry.path().strip_prefix(root).unwrap().to_owned(), None);
                visit(root, &entry.path(), files);
            } else {
                files.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    Some(fs::read(entry.path()).unwrap()),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    for directory in ["config", "data", "cache", "runtime"] {
        visit(root, &root.join(directory), &mut files);
    }
    files
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
    for directory in ["data/vosu", "cache/vosu", "runtime/vosu"] {
        let path = root.path().join(directory);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("preserve-6187"),
            format!("source {directory} 6187"),
        )
        .unwrap();
    }
    let before = normal_inventory(root.path());
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
    assert_eq!(
        normal_inventory(root.path()),
        before,
        "first start must preserve all normal-root bytes and inventory"
    );

    fs::write(normal.join("vosu.toml"), "POLL_RATE = [broken on restart").unwrap();
    let restart_source = normal_inventory(root.path());
    let restarted = spawn(root.path());
    bounded!(reachable(port));
    assert_eq!(fs::read_to_string(alpha).unwrap(), imported);
    assert_eq!(
        fs::read_to_string(normal.join("vosu.toml")).unwrap(),
        "POLL_RATE = [broken on restart"
    );
    assert_eq!(
        normal_inventory(root.path()),
        restart_source,
        "restart must not write normal roots or re-import their poisoned config"
    );
    drop(restarted);
}
