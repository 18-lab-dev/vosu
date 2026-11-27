struct ChildGuard(Option<Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = vosu_proptest::wait::child(child);
        }
    }
}

#[cfg(any(target_os = "linux", feature = "test-harness"))]
async fn wait_get(client: &reqwest::Client, url: &str) -> reqwest::Response {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match bounded!(client.get(url).send()) {
            Ok(response) => return response,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("daemon did not become reachable at {url}: {error}"),
        }
    }
}

#[cfg(any(target_os = "linux", feature = "test-harness"))]
fn available_port() -> u16 {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[cfg(target_os = "linux")]
fn legacy_config(port: u16, static_root: &Path) -> String {
    format!(
        "ENABLE_AUTOUPDATE=false\nOPEN_DASHBOARD_ON_STARTUP=false\nDEBUG_LOG=false\nPOLL_RATE=150\nPRECISE_DATA_POLL_RATE=10\nSERVER_IP=127.0.0.1\nSERVER_PORT={port}\nDASHBOARD_IP=127.0.0.1\nDASHBOARD_PORT=0\nSTATIC_FOLDER_PATH={}\nALLOWED_IPS=127.0.0.1,localhost,absolute\n",
        static_root.display(),
    )
}

#[cfg(any(target_os = "linux", feature = "test-harness"))]
fn toml_config(port: u16, _static_root: &Path) -> String {
    format!(
        "ENABLE_AUTOUPDATE = false\nOPEN_DASHBOARD_ON_STARTUP = false\nDEBUG_LOG = false\nSERVER_IP = \"127.0.0.1\"\nSERVER_PORT = {port}\nDASHBOARD_IP = \"127.0.0.1\"\nDASHBOARD_PORT = 0\nALLOWED_IPS = \"127.0.0.1,localhost,absolute\"\n"
    )
}

#[cfg(target_os = "linux")]
fn write_overlay_root_plugin_setting(config_dir: &Path, static_root: &Path) {
    let plugins_dir = config_dir.join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    fs::write(
        plugins_dir.join("settings.json"),
        serde_json::json!({
            "version": 1,
            "plugins": {
                "vosu.overlayhost": {
                    "static_folder_path": static_root.to_string_lossy()
                }
            }
        })
        .to_string(),
    )
    .unwrap();
}

#[cfg(any(target_os = "linux", feature = "test-harness"))]
fn smoke_channel() -> vosu_sys::platform::BuildChannel {
    let binary = std::env::var_os("VOSU_RELEASE_SMOKE_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_vosu").into());
    let mut child = Command::new(binary)
        .arg("--version")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let status = vosu_proptest::wait::child(&mut child).unwrap();
    assert!(status.success());
    let mut version = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut version).unwrap();
    vosu_sys::platform::BuildChannel::from_version(version.split_whitespace().last().unwrap()).unwrap()
}

/// Windows portable data live by the executable, so stage a fresh hard link
/// (or copy across volumes). The broker's LOCALAPPDATA is also private.
#[cfg(feature = "test-harness")]
fn isolated_daemon(root: &Path) -> (Command, PathBuf) {
    let binary = Path::new(env!("CARGO_BIN_EXE_vosu"));
    let mut command = if cfg!(windows) {
        let program = root.join("bin");
        fs::create_dir(&program).unwrap();
        let copy = program.join(binary.file_name().unwrap());
        if fs::hard_link(binary, &copy).is_err() {
            fs::copy(binary, &copy).unwrap();
        }
        Command::new(copy)
    } else {
        Command::new(binary)
    };
    for (variable, directory) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("LOCALAPPDATA", "local"),
        ("TEMP", "temp"),
        ("OneDrive", "onedrive"),
    ] {
        let path = root.join(directory);
        fs::create_dir_all(&path).unwrap();
        command.env(variable, path);
    }
    command
        .current_dir(root)
        .arg("--update=false")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("VOSU_DASHBOARD")
        .env_remove("VOSU_TEST_OFFICIAL_PLUGIN")
        .env("VOSU_TEST_ISOLATED_DISCOVERY", "1")
        .env("VOSU_TEST_STOP_FILE", root.join("stop"));
    let parent = if cfg!(windows) {
        root.join("bin")
    } else {
        root.join("config/vosu")
    };
    (command, smoke_channel().root(parent))
}

#[cfg(not(feature = "test-harness"))]
#[test]
fn official_fixture_switch_refuses_an_actual_nonharness_daemon_before_startup() {
    let root = TempDir::new().unwrap();
    let binary = std::env::var_os("VOSU_PLUGIN_APPROVAL_DAEMON")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_vosu").into());
    let mut child = ChildGuard(Some(
        Command::new(binary)
            .current_dir(root.path())
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("absent"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env("LOCALAPPDATA", root.path().join("local"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env("VOSU_TEST_OFFICIAL_PLUGIN", root.path().join("absent"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let process = child.0.as_mut().unwrap();
    let diagnostics = process.stderr.take().unwrap();
    let status = vosu_proptest::wait::child(process).unwrap();
    assert!(!status.success());
    let diagnostic = std::io::read_to_string(diagnostics).unwrap();
    assert!(
        diagnostic.contains("VOSU_TEST_OFFICIAL_PLUGIN requires a debug build with test-harness"),
        "{diagnostic}"
    );
}

#[cfg(target_os = "linux")]
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

