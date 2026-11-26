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
    let executable = env!("CARGO_PKG_VERSION");
    assert_eq!(current_version().to_string(), executable);
    assert_eq!(
        crate::updater::user_agent(&current_version()),
        format!("vosu/{executable}")
    );

    let options = daemon_logger_options(false);
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
