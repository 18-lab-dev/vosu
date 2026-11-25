//! One real `vosu` daemon under test, with `--plugin` packages, on its own
//! roots and ports, driven through its HTTP listeners exactly as the
//! dashboard drives them.
//!
//! The daemon is the executable `VOSU_PLUGIN_APPROVAL_DAEMON` names (the
//! prerelease release build without `test-harness` that
//! `scripts/plugin-host-fixtures.py --release-daemon` stages), else this
//! checkout's own debug `vosu`. Both refuse to run a `--plugin` process
//! package before approval; only the release build proves it without the
//! test harness, so each run prints which one it used.

use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Startup, including a debug daemon's, on a loaded machine.
const STARTUP: Duration = Duration::from_secs(60);

/// The environment variable naming the release daemon.
pub const RELEASE_DAEMON: &str = "VOSU_PLUGIN_APPROVAL_DAEMON";
/// The environment variable naming a stable (non-prerelease) release daemon.
pub const STABLE_DAEMON: &str = "VOSU_PLUGIN_APPROVAL_STABLE_DAEMON";

/// The daemon executable for the ordinary approval tests and whether it is
/// the staged release build.
pub fn executable() -> (PathBuf, bool) {
    match std::env::var_os(RELEASE_DAEMON) {
        Some(path) => {
            let path = PathBuf::from(path);
            assert!(
                path.is_file(),
                "{RELEASE_DAEMON}={} is not a file",
                path.display()
            );
            println!("RECORD approval daemon=release path={}", path.display());
            (path, true)
        }
        None => {
            let path = PathBuf::from(env!("CARGO_BIN_EXE_vosu"));
            println!(
                "RECORD approval daemon=debug test-harness path={} ({RELEASE_DAEMON} unset)",
                path.display()
            );
            (path, false)
        }
    }
}

/// `vosu --version`'s semver.
pub fn version(executable: &Path) -> semver::Version {
    let output = Command::new(executable)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} --version failed",
        executable.display()
    );
    let text = String::from_utf8(output.stdout).unwrap();
    semver::Version::parse(text.split_whitespace().last().unwrap()).unwrap()
}

/// What an HTTP mutation presents (catalog-v2.md A5.5 "Management routes").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presented {
    /// The dashboard's own origin and this launch's `X-Vosu-CSRF`.
    Dashboard,
    /// The dashboard's origin without a CSRF token.
    NoToken,
    /// The dashboard's origin with another token.
    WrongToken,
    /// This launch's token from another origin.
    ForeignOrigin,
    /// Dashboard origin and token, sent to the overlay listener.
    OverlayListener,
}

pub struct Daemon {
    child: Option<Child>,
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
    base: String,
    dashboard: String,
    csrf: String,
    log: PathBuf,
    config: PathBuf,
}

fn available_port() -> u16 {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Daemon {
    /// Starts `executable` with one `--plugin` per entry of `plugins`, on
    /// roots under `root` (reused by a later start under the same root, so
    /// a restart keeps its approvals). Returns once the dashboard answered.
    pub fn start(root: &Path, executable: &Path, plugins: &[&Path]) -> Self {
        let home = root.join("home");
        // A release daemon owns its program directory (the updater's owner
        // lock), and Windows keeps a portable install's config beside the
        // executable, so each root runs its own link or copy.
        let bin = home.join("bin");
        let copy = bin.join(executable.file_name().unwrap());
        if !copy.exists() {
            std::fs::create_dir_all(&bin).unwrap();
            if std::fs::hard_link(executable, &copy).is_err() {
                std::fs::copy(executable, &copy).unwrap();
            }
        }
        let executable = copy;
        #[cfg(windows)]
        let config = bin;
        #[cfg(not(windows))]
        let config = home.join("config/vosu");
        let channel =
            vosu_sys::platform::BuildChannel::from_version(&version(&executable).to_string())
                .unwrap();
        let config = channel.root(config);
        std::fs::create_dir_all(&config).unwrap();
        let port = available_port();
        std::fs::write(
            config.join("vosu.toml"),
            format!(
                "ENABLE_AUTOUPDATE = false\nOPEN_DASHBOARD_ON_STARTUP = false\nDEBUG_LOG = false\nSERVER_IP = \"127.0.0.1\"\nSERVER_PORT = {port}\nDASHBOARD_IP = \"127.0.0.1\"\nDASHBOARD_PORT = 0\nALLOWED_IPS = \"127.0.0.1,localhost,absolute\"\n"
            ),
        )
        .unwrap();
        let log = root.join(format!("daemon-{port}.log"));
        let output = std::fs::File::create(&log).unwrap();
        let mut command = Command::new(&executable);
        command.arg("--update=false");
        for plugin in plugins {
            command.arg("--plugin").arg(plugin);
        }
        for (variable, directory) in [
            ("HOME", ""),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ] {
            let path = home.join(directory);
            std::fs::create_dir_all(&path).unwrap();
            command.env(variable, path);
        }
        command
            // The Windows archive and OneDrive guards must not mistake the
            // scratch root for the user's TEMP or OneDrive tree.
            .env("TEMP", home.join("runtime-temp-sentinel"))
            .env("OneDrive", home.join("onedrive-sentinel"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output);
        let child = command.spawn().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let mut daemon = Self {
            child: Some(child),
            runtime,
            client,
            base: format!("http://127.0.0.1:{port}"),
            dashboard: String::new(),
            csrf: String::new(),
            log,
            config,
        };
        daemon.wait_for_dashboard();
        daemon
    }

    /// The overlay listener redirects `/` to the dashboard, whose page
    /// carries this launch's CSRF token.
    fn wait_for_dashboard(&mut self) {
        let deadline = Instant::now() + STARTUP;
        let url = format!("{}/", self.base);
        let (origin, page) = loop {
            let fetched = self.runtime.block_on(async {
                let response = self.client.get(&url).send().await?;
                let origin = response.url().origin().ascii_serialization();
                Ok::<_, reqwest::Error>((origin, response.text().await?))
            });
            match fetched {
                Ok(found) => break found,
                Err(error) => {
                    if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                        panic!("the daemon exited with {status}:\n{}", self.log());
                    }
                    assert!(
                        Instant::now() < deadline,
                        "the daemon never answered at {url}: {error}\n{}",
                        self.log()
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        };
        self.csrf = page
            .split("\"csrf\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_else(|| panic!("no CSRF token on the dashboard page"))
            .to_owned();
        assert_ne!(
            origin, self.base,
            "the dashboard is not a separate listener"
        );
        self.dashboard = origin;
    }

    /// The daemon's combined output so far.
    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Waits up to `bound` for the output to contain `text`.
    pub fn wait_for_log(&self, text: &str, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        loop {
            if self.log().contains(text) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    /// The approval store's file (catalog-v2.md A5.5 "Persistence").
    pub fn approvals(&self) -> PathBuf {
        self.config
            .join(vosu_core::plugin::approval::APPROVALS_FILE)
    }

    /// `GET /api/plugins` on the dashboard listener.
    pub fn plugins(&self) -> Vec<Value> {
        let (status, body) = self.request(
            reqwest::Method::GET,
            "/api/plugins",
            None,
            Presented::Dashboard,
        );
        assert_eq!(status, 200, "{body}");
        let list: Value = serde_json::from_str(&body).unwrap();
        list["plugins"].as_array().unwrap().clone()
    }

    /// The listed entry whose `id` is `id`.
    pub fn plugin(&self, id: &str) -> Value {
        self.plugins()
            .into_iter()
            .find(|plugin| plugin["id"] == id)
            .unwrap_or_else(|| panic!("{id} is not listed:\n{}", self.log()))
    }

    /// One request: the status and body text.
    pub fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        presented: Presented,
    ) -> (u16, String) {
        let base = match presented {
            Presented::OverlayListener => &self.base,
            _ => &self.dashboard,
        };
        let mut request = self.client.request(method, format!("{base}{path}")).header(
            reqwest::header::ORIGIN,
            match presented {
                Presented::ForeignOrigin => "http://attacker.invalid",
                _ => self.dashboard.as_str(),
            },
        );
        match presented {
            Presented::NoToken => {}
            Presented::WrongToken => request = request.header("X-Vosu-CSRF", "0".repeat(64)),
            _ => request = request.header("X-Vosu-CSRF", &self.csrf),
        }
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string());
        }
        self.runtime.block_on(async {
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            (status, response.text().await.unwrap())
        })
    }

    /// A dashboard-authorized `POST`/`DELETE` that must succeed: the
    /// plugin's view afterwards.
    pub fn act(
        &self,
        method: reqwest::Method,
        id: &str,
        action: &str,
        body: Option<&Value>,
    ) -> Value {
        let (status, text) = self.request(
            method,
            &format!("/api/plugins/{id}/{action}"),
            body,
            Presented::Dashboard,
        );
        assert_eq!(status, 200, "{action}: {text}\n{}", self.log());
        serde_json::from_str(&text).unwrap()
    }

    /// Kills the daemon (its plugin hosts die with it: parent-death signal
    /// on Linux, kill-on-close job on Windows) and reaps it.
    pub fn kill(mut self) {
        self.reap();
    }

    fn reap(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = vosu_proptest::wait::child(&mut child);
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.reap();
    }
}
