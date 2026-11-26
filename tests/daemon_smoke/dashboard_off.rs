//! A7.3 dashboard off against the real executable: `VOSU_DASHBOARD=off` or
//! `ENABLE_DASHBOARD = false` binds no dashboard listener while overlays and
//! API v1 keep serving; any other `VOSU_DASHBOARD` value fails startup; the
//! dashboard cannot turn itself off, and a live edit of the key needs a
//! restart. Discovery is isolated, so no real game process is ever read.
use super::*;

const ISOLATED_DISCOVERY_ENV: &str = "VOSU_TEST_ISOLATED_DISCOVERY";
const RESTART_NOTICE: &str = "ENABLE_DASHBOARD changed in vosu.toml; restart vosu to apply it";

/// Fields drop in order: the daemon is killed before its root is removed.
struct Run {
    child: ChildGuard,
    root: TempDir,
    port: u16,
    dashboard_port: u16,
    config: PathBuf,
    client: reqwest::Client,
}

impl Run {
    fn spawn(environment: Option<&str>, enable_dashboard: Option<bool>) -> Self {
        let root = TempDir::new().unwrap();
        let config_dir = smoke_channel().root(root.path().join("config/vosu"));
        fs::create_dir_all(&config_dir).unwrap();
        let (port, dashboard_port) = (available_port(), available_port());
        let mut text = toml_config(port, &root.path().join("static")).replace(
            "DASHBOARD_PORT = 0",
            &format!("DASHBOARD_PORT = {dashboard_port}"),
        );
        if let Some(enabled) = enable_dashboard {
            text.push_str(&format!("ENABLE_DASHBOARD = {enabled}\n"));
        }
        let config = config_dir.join("vosu.toml");
        fs::write(&config, text).unwrap();
        let log = fs::File::create_new(root.path().join("daemon.log")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_vosu"));
        command
            .arg("--update=false")
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
            .env("VOSU_PROCESS_EVENTS", "poll")
            .env(ISOLATED_DISCOVERY_ENV, "1")
            .env_remove("VOSU_DASHBOARD")
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        if let Some(value) = environment {
            command.env("VOSU_DASHBOARD", value);
        }
        let child = ChildGuard(Some(command.spawn().unwrap()));
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        Self {
            child,
            root,
            port,
            dashboard_port,
            config,
            client,
        }
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn dashboard(&self) -> String {
        format!("http://127.0.0.1:{}", self.dashboard_port)
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.path().join("daemon.log")).unwrap()
    }

    async fn status(&self, url: &str) -> u16 {
        bounded!(self.client.get(url).send())
            .unwrap()
            .status()
            .as_u16()
    }

    async fn wait_for_log(&self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.log().contains(needle) {
            assert!(
                Instant::now() < deadline,
                "daemon never logged {needle:?}:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[tokio::test]
async fn dashboard_off_binds_no_dashboard_listener_and_keeps_overlays_and_api_v1() {
    for (environment, enable_dashboard) in [(Some("off"), None), (None, Some(false))] {
        let run = Run::spawn(environment, enable_dashboard);
        let ready = bounded!(wait_get(&run.client, &format!("{}/json", run.base())));
        assert_ne!(ready.status(), 404);
        run.wait_for_log("Dashboard is off: vosu.dashboard is inactive (dashboard_disabled)")
            .await;

        // Nothing listens on the configured dashboard port.
        assert!(
            TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, run.dashboard_port)).is_ok(),
            "{environment:?}/{enable_dashboard:?}: the dashboard port is bound"
        );
        assert!(!run.log().contains("Dashboard: http"), "{}", run.log());
        // No discovery of, or redirect to, a dead dashboard.
        for path in ["/api/dashboard", "/", "/settings", "/available", "/tools"] {
            assert_eq!(
                run.status(&format!("{}{path}", run.base())).await,
                404,
                "{path}"
            );
        }
        // Overlays and API v1 are unaffected.
        assert_eq!(
            run.status(&format!("{}/api/v1/sessions", run.base())).await,
            200
        );
        // The tool host keeps the design-system set on the overlay role.
        for path in ["/assets/tokens.css", "/favicon.ico"] {
            assert_eq!(
                run.status(&format!("{}{path}", run.base())).await,
                200,
                "{path}"
            );
        }
        // The environment override never rewrites the configured key.
        let persisted: toml::Table =
            toml::from_str(&fs::read_to_string(&run.config).unwrap()).unwrap();
        assert_eq!(
            persisted
                .get("ENABLE_DASHBOARD")
                .and_then(toml::Value::as_bool),
            Some(enable_dashboard.unwrap_or(true))
        );
    }
}

#[tokio::test]
async fn any_other_vosu_dashboard_value_fails_startup_before_binding() {
    for value in ["", "on", "OFF", "false"] {
        let mut run = Run::spawn(Some(value), None);
        let mut child = run.child.0.take().unwrap();
        let status = tokio::task::spawn_blocking(move || vosu_proptest::wait::child(&mut child))
            .await
            .unwrap()
            .unwrap();
        assert!(!status.success(), "{value:?}");
        let log = run.log();
        assert!(
            log.contains("VOSU_DASHBOARD must be unset or exactly `off`"),
            "{value:?}: {log}"
        );
        assert!(!log.contains("Overlays and API"), "{value:?}: {log}");
        for port in [run.port, run.dashboard_port] {
            assert!(TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_ok());
        }
    }
}

#[tokio::test]
async fn the_dashboard_cannot_turn_itself_off_and_a_live_edit_needs_a_restart() {
    let run = Run::spawn(None, None);
    let dashboard = run.dashboard();
    let page = bounded!(wait_get(&run.client, &format!("{dashboard}/")));
    assert_eq!(page.status(), 200);
    let page = bounded!(page.text()).unwrap();
    let csrf = page
        .split("\"csrf\":\"")
        .nth(1)
        .expect("dashboard CSRF context")
        .split('"')
        .next()
        .unwrap()
        .to_owned();
    assert!(run.log().contains(&format!("Dashboard: {dashboard}/")));

    // The dashboard listener never serves API v1.
    assert_eq!(
        run.status(&format!("{dashboard}/api/v1/sessions")).await,
        404
    );

    // A correctly authorized save naming the switch publishes nothing.
    let before = fs::read(&run.config).unwrap();
    let refused = bounded!(
        run.client
            .post(format!("{dashboard}/api/settingsSave"))
            .header(reqwest::header::ORIGIN, &dashboard)
            .header("X-Vosu-CSRF", &csrf)
            .json(&serde_json::json!({"ENABLE_DASHBOARD": false, "DEBUG_LOG": true}))
            .send()
    )
    .unwrap();
    assert_eq!(refused.status(), 400);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(fs::read(&run.config).unwrap(), before);

    // Editing the file is logged as needing a restart and is not applied.
    let edited = String::from_utf8(before)
        .unwrap()
        .replace("ENABLE_DASHBOARD = true", "ENABLE_DASHBOARD = false");
    assert!(edited.contains("ENABLE_DASHBOARD = false"));
    fs::write(&run.config, edited).unwrap();
    run.wait_for_log(RESTART_NOTICE).await;
    assert_eq!(run.status(&format!("{dashboard}/")).await, 200);
    assert_eq!(
        run.status(&format!("{}/api/dashboard", run.base())).await,
        200
    );
}
