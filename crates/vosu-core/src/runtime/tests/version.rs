struct FixtureHosts;
impl BundledHosts for FixtureHosts {
    fn startup_flags(&self, _: &[OsString]) -> crate::app::StartupFlags { unreachable!() }
    fn poll_duration(&self, _: f64) -> Duration { unreachable!() }
    fn projections(&self, _: &AdapterRequesters) -> Projections { unreachable!() }
    fn adapter_routes(
        &self, _: [Arc<dyn crate::server::routes::StateJsonProvider>; 4],
        _: [WebSocketHub; 4], _: Arc<dyn vosu_surface::requester::BundledRequester>,
    ) -> [HostRoutes; 3] { unreachable!() }
    fn dashboard_routes(&self, _: &str) -> HostRoutes { unreachable!() }
    fn mount_tool_host(
        &self, _: Arc<dyn vosu_surface::requester::BundledRequester>, _: vosu_surface::tool_view::ToolHostViews,
        _: &[vosu_surface::listener::ListenerRole],
    ) -> Result<Box<dyn Send + Sync>, String> { unreachable!() }
    fn overlay(&self, _: OverlayInputs) -> Arc<dyn OverlayComposition> { unreachable!() }
}

static VERSION_FIXTURE: Bundle = Bundle {
    version: "23.0.0-beta.5",
    plugin_host_identity: "fixture-host",
    plugins: &[],
    plugin_ids: &[],
    dashboard: "fixture.dashboard",
    tool_host: "fixture.toolhost",
    overlay_root: OverlayRootSetting { plugin: "fixture.overlayhost", key: "fixture.root" },
    hosts: &FixtureHosts,
};

#[derive(Default)]
struct VersionProbeConsole(std::sync::Mutex<Vec<String>>);

impl crate::logging::ConsoleSink for VersionProbeConsole {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, text: &str) -> io::Result<()> {
        self.0.lock().unwrap().push(text.to_owned());
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
}

/// Regression: vosu-core once reported its own 0.1.0 package version, so an
/// alpha saw stable 1.2.5 as a major update, sent `vosu/0.1.0` to GitHub and
/// prefixed every log line with 0.1.0.
#[tokio::test]
async fn executable_version_is_the_only_reported_version() {
    let executable = VERSION_FIXTURE.version;
    assert_eq!(VERSION_FIXTURE.version().to_string(), executable);
    assert_eq!(
        crate::updater::user_agent(&VERSION_FIXTURE.version()),
        format!("vosu/{executable}")
    );

    let options = daemon_logger_options(&VERSION_FIXTURE, false);
    assert_eq!(options.version, executable);
    let root = tempfile::tempdir().unwrap();
    let console = Arc::new(VersionProbeConsole::default());
    let logger = Logger::new(root.path(), options, console.clone()).unwrap();
    logger.info("version prefix probe");
    bounded!(logger.flush()).unwrap();
    let lines = console.0.lock().unwrap().clone();
    let line = lines
        .iter()
        .find(|line| line.contains("version prefix probe"))
        .expect("probe line reaches the console");
    let prefix = &line[..line.find("version prefix probe").unwrap()];
    assert!(
        prefix.contains(executable),
        "console line is not prefixed with {executable}: {line:?}"
    );
    bounded!(logger.shutdown()).unwrap();
}
