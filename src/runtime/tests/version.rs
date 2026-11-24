// The root package is the only app-version authority. `vosu-core` has its own
// unrelated package version (0.1.0), which 1.2.5 leaked into the dashboard,
// console log prefixes, and catalog requests; each test pins one of those
// consumers to the executable's own `CARGO_PKG_VERSION`.

#[test]
fn current_version_is_the_root_package_version() {
    assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        current_version(),
        semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
    );
}

#[test]
fn dashboard_and_settings_render_the_root_package_version() {
    let dashboard = render_homepage(
        PageService.dashboard_template(),
        DynamicPage::Dashboard,
        Some(0),
        Some(0),
        "",
        "",
        None,
    );
    let dashboard = String::from_utf8(dashboard.body.to_vec()).unwrap();
    assert!(
        dashboard.contains(&format!(
            r#"<span class="status-value">{}</span>"#,
            env!("CARGO_PKG_VERSION")
        )),
        "dashboard status must show the executable version"
    );

    let settings = render_runtime_settings(&Config::default());
    assert!(
        settings.contains(&format!(
            r#"<span class="settings-item__control settings-item__value">vosu {}</span>"#,
            env!("CARGO_PKG_VERSION")
        )),
        "settings About must show the executable version"
    );
}

#[test]
fn production_catalog_identifies_the_root_package_version() {
    assert_eq!(
        production_catalog().user_agent(),
        format!(
            "vosu/{} (+https://github.com/18-lab-dev/vosu)",
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[derive(Default)]
struct CapturedConsole(Mutex<String>);

impl vosu_core::logging::ConsoleSink for CapturedConsole {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, text: &str) -> io::Result<()> {
        self.0.lock().unwrap().push_str(text);
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_console_lines_are_prefixed_with_the_root_package_version() {
    let logs = tempfile::tempdir().unwrap();
    let console = Arc::new(CapturedConsole::default());
    let logger = Logger::new(
        logs.path(),
        production_logger_options(false),
        console.clone(),
    )
    .unwrap();
    logger.info("ready");
    logger.flush().await.unwrap();
    logger.shutdown().await.unwrap();

    let line = console.0.lock().unwrap().clone();
    // The prefix is the version in grey (`\x1b[38;2;170;170;170m…\x1b[0m`).
    let prefix = format!("\x1b[38;2;170;170;170m{}\x1b[0m  ", env!("CARGO_PKG_VERSION"));
    assert!(
        line.starts_with(&prefix),
        "console line must start with the executable version: {line:?}"
    );
}
