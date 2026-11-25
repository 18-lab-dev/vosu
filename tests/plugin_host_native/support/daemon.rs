//! The real `vosu` daemon as core (M5 N0): a staged testkit package loaded
//! through `--plugin` with an approval record written before start, under
//! isolated configuration roots.
//!
//! This drives catalog-v2.md A5.5's approval-gated `--plugin` launch (M5
//! U0): the record in `plugins/approvals.json` is the operator's explicit
//! approval of exactly the staged manifest and library bytes, and the
//! daemon's own approval gate checks it on every start.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::artifacts::Staged;

/// One daemon's roots and listener.
#[derive(Debug)]
pub struct Daemon {
    /// The executable started: on Windows a copy inside the run's root,
    /// because Windows keeps configuration next to the executable.
    pub executable: PathBuf,
    pub port: u16,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The configuration root `binary` uses, including the prerelease
/// subdirectory of a prerelease version (D21).
fn channel_root(normal: PathBuf) -> PathBuf {
    if env!("CARGO_PKG_VERSION").contains('-') {
        normal.join("prerelease")
    } else {
        normal
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Prepares `root` and returns the command that starts `binary` as a
/// daemon loading `staged` through `--plugin`, plus its listener.
pub fn command(binary: &Path, root: &Path, staged: &Staged) -> (Command, Daemon) {
    #[cfg(windows)]
    let (executable, config) = {
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let executable = bin.join(binary.file_name().unwrap());
        std::fs::copy(binary, &executable).unwrap();
        (executable, bin)
    };
    #[cfg(target_os = "linux")]
    let (executable, config) = (binary.to_owned(), root.join("config/vosu"));
    let config = channel_root(config);
    std::fs::create_dir_all(config.join("plugins")).unwrap();
    let port = free_port();
    std::fs::write(
        config.join("vosu.toml"),
        format!(
            "SERVER_IP = \"127.0.0.1\"\nSERVER_PORT = {port}\nDASHBOARD_PORT = 0\n\
             ENABLE_AUTOUPDATE = false\nOPEN_DASHBOARD_ON_STARTUP = false\n"
        ),
    )
    .unwrap();
    let manifest = std::fs::read(staged.dir.join("manifest.toml")).unwrap();
    let id = manifest_id(&String::from_utf8_lossy(&manifest));
    let provenance = std::fs::canonicalize(&staged.dir).unwrap();
    let approvals = serde_json::json!({
        "version": 1,
        "approvals": [{
            "id": id,
            "runtime": "process",
            "manifest_sha256": sha256(&manifest),
            "library_sha256": staged.sha256,
            "provenance": {"kind": "development", "path": provenance},
        }],
    });
    std::fs::write(
        config.join("plugins/approvals.json"),
        serde_json::to_vec_pretty(&approvals).unwrap(),
    )
    .unwrap();

    let mut command = Command::new(&executable);
    command
        .arg("--update=false")
        .arg("--plugin")
        .arg(&staged.dir)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        // Keep the Windows archive and OneDrive guards from mistaking the
        // run's root for the user's TEMP or OneDrive tree (release smoke).
        .env("TEMP", root.join("runtime-temp-sentinel"))
        .env("TMP", root.join("runtime-temp-sentinel"))
        .env("OneDrive", root.join("onedrive-sentinel"))
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(root.join("daemon.stdout.log")).unwrap())
        .stderr(std::fs::File::create(root.join("daemon.stderr.log")).unwrap());
    (command, Daemon { executable, port })
}

fn manifest_id(manifest: &str) -> String {
    manifest
        .lines()
        .find_map(|line| {
            line.strip_prefix("id = ")
                .map(|value| value.trim().trim_matches(['\'', '"']).to_owned())
        })
        .expect("the staged manifest names its id")
}

impl Daemon {
    /// The management view of plugin `id` (`GET /api/plugins`), once the
    /// listener answers.
    pub fn plugin(&self, id: &str) -> Option<serde_json::Value> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let url = format!("http://127.0.0.1:{}/api/plugins", self.port);
        let listing: serde_json::Value = runtime.block_on(async {
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .ok()?;
            let response = client.get(&url).send().await.ok()?;
            response.json().await.ok()
        })?;
        listing["plugins"]
            .as_array()?
            .iter()
            .find(|plugin| plugin["id"] == id)
            .cloned()
    }

    /// Waits up to `bound` for plugin `id`'s view to satisfy `accept`.
    pub fn wait_plugin(
        &self,
        id: &str,
        bound: Duration,
        accept: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + bound;
        let mut last = None;
        loop {
            if let Some(view) = self.plugin(id) {
                if accept(&view) {
                    return view;
                }
                last = Some(view);
            }
            assert!(
                Instant::now() < deadline,
                "plugin {id} never reached the expected state; last view {last:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
