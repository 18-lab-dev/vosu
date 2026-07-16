use chrono::{DateTime, Local};

use super::*;
use std::fs::File;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::tempdir;

#[derive(Default)]
struct TestConsole {
    tty: bool,
    logs: Mutex<Vec<String>>,
    progress: Mutex<Vec<String>>,
    clears: AtomicUsize,
}

impl TestConsole {
    fn tty() -> Self {
        Self {
            tty: true,
            ..Self::default()
        }
    }
}

impl ConsoleSink for TestConsole {
    fn is_tty(&self) -> bool {
        self.tty
    }
    fn write_log(&self, text: &str) -> io::Result<()> {
        lock(&self.logs).push(text.to_owned());
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> io::Result<()> {
        self.clears.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn write_progress(&self, text: &str) -> io::Result<()> {
        lock(&self.progress).push(text.to_owned());
        Ok(())
    }
}

fn options(capacity: usize) -> LoggerOptions {
    LoggerOptions {
        version: "0.1.0".to_owned(),
        debug_log: false,
        queue_capacity: capacity,
    }
}

#[tokio::test]
async fn exact_file_labels_and_highlights_are_stripped() {
    let dir = tempdir().unwrap();
    let console = Arc::new(TestConsole::default());
    let logger = Logger::new(dir.path(), options(32), console).unwrap();
    for level in [
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
        LogLevel::DebugError,
        LogLevel::Debug,
        LogLevel::Time,
    ] {
        logger.log(level, "plain %bright% tail");
    }
    logger.flush().await.unwrap();
    let text = fs::read_to_string(dir.path().join("latest.log")).unwrap();
    let labels: Vec<_> = text.lines().map(|line| &line[13..21]).collect();
    assert_eq!(
        labels,
        [
            "[info]  ", "[warn]  ", "[error] ", "[derror]", "[debug] ", "[time]  "
        ]
    );
    assert!(text.lines().all(|line| line.ends_with("plain bright tail")));
    assert!(!text.contains('%'));
    logger.shutdown().await.unwrap();
}

#[tokio::test]
async fn console_uses_exact_ansi_and_debug_reload() {
    let dir = tempdir().unwrap();
    let console = Arc::new(TestConsole::default());
    let logger = Logger::new(dir.path(), options(8), console.clone()).unwrap();
    logger.debug("hidden %value%");
    assert!(lock(&console.logs).is_empty());
    let mut config = crate::config::Config {
        debug_log: true,
        ..Default::default()
    };
    logger.update_config(&config);
    logger.debug("shown %value%");
    config.debug_log = false;
    logger.update_config(&config);
    logger.time("hidden again");
    {
        let logs = lock(&console.logs);
        assert_eq!(logs.len(), 1);
        assert!(logs[0].starts_with("\x1b[38;2;170;170;170m0.1.0\x1b[0m  \x1b[38;2;120;120;120m┃\x1b[0m  \x1b[38;2;170;170;170m"));
        assert!(logs[0].ends_with("\x1b[0m  shown \x1b[1m\x1b[37mvalue\x1b[0m\n"));
    }
    logger.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn bounded_queue_drops_newest_without_blocking() {
    let dir = tempdir().unwrap();
    let logger = Logger::new(dir.path(), options(1), Arc::new(TestConsole::default())).unwrap();
    for n in 0..1000 {
        logger.debug(format!("line {n}"));
    }
    assert!(logger.dropped_lines() >= 999);
    logger.flush().await.unwrap();
    logger.shutdown().await.unwrap();
}

#[test]
fn running_time_has_source_shape_and_unbounded_hours() {
    assert_eq!(
        format_running_time(Duration::from_millis(0)),
        "00:00:00.000"
    );
    assert_eq!(
        format_running_time(Duration::from_millis(3_661_007)),
        "01:01:01.007"
    );
    assert_eq!(
        format_running_time(Duration::from_millis(360_000_999)),
        "100:00:00.999"
    );
}

#[test]
fn rotation_uses_mtime_and_collision_suffix() {
    let dir = tempdir().unwrap();
    let latest = dir.path().join("latest.log");
    fs::write(&latest, "old").unwrap();
    let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    File::options()
        .write(true)
        .open(&latest)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let local: DateTime<Local> = modified.into();
    let base = local.format("%Y-%m-%d %H-%M-%S").to_string();
    fs::write(dir.path().join(format!("{base}.log")), "collision").unwrap();
    let rotated = rotate_latest(dir.path()).unwrap().unwrap();
    assert_eq!(
        rotated.file_name().unwrap().to_string_lossy(),
        format!("{base}-1.log")
    );
    assert_eq!(fs::read_to_string(rotated).unwrap(), "old");
}

#[test]
fn cleanup_triggers_at_100_mib_and_stops_at_90_mib() {
    let dir = tempdir().unwrap();
    let base = SystemTime::now() - Duration::from_secs(100);
    for (index, mib) in [8_u64, 8, 80, 4].into_iter().enumerate() {
        let path = if index == 3 {
            dir.path().join("latest.log")
        } else {
            dir.path().join(format!("{index}.log"))
        };
        let file = File::create(path).unwrap();
        file.set_len(mib * 1024 * 1024).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(base + Duration::from_secs(index as u64)))
            .unwrap();
    }
    let report = cleanup_logs(dir.path()).unwrap();
    assert_eq!(report.deleted_files, 2);
    assert_eq!(report.freed_bytes, 16 * 1024 * 1024);
    assert!(!dir.path().join("0.log").exists());
    assert!(!dir.path().join("1.log").exists());
    assert!(dir.path().join("2.log").exists());
    assert!(dir.path().join("latest.log").exists());
}

#[tokio::test]
async fn progress_is_tty_only_and_rate_limited() {
    let dir = tempdir().unwrap();
    let non_tty = Arc::new(TestConsole::default());
    let logger = Logger::new(dir.path(), options(8), non_tty.clone()).unwrap();
    let token = logger.progress_start("Download");
    logger.progress_update(token, 0.1, "one");
    assert!(lock(&non_tty.progress).is_empty());
    logger.shutdown().await.unwrap();

    let dir = tempdir().unwrap();
    let tty = Arc::new(TestConsole::tty());
    let logger = Logger::new(dir.path(), options(8), tty.clone()).unwrap();
    let token = logger.progress_start("Download");
    let after_start = lock(&tty.progress).len();
    logger.progress_update(token, 0.1, "one");
    let after_first = lock(&tty.progress).len();
    logger.progress_update(token, 0.2, "two");
    assert_eq!(lock(&tty.progress).len(), after_first);
    assert!(after_first > after_start);
    logger.progress_update(token, 1.0, "done");
    assert_eq!(lock(&tty.progress).len(), after_first + 1);
    logger.shutdown().await.unwrap();
}

#[tokio::test]
async fn flush_acknowledges_persisted_lines_and_shutdown_is_idempotent() {
    let dir = tempdir().unwrap();
    let logger = Logger::new(dir.path(), options(8), Arc::new(TestConsole::default())).unwrap();
    logger.info("persist me");
    logger.flush().await.unwrap();
    assert!(
        fs::read_to_string(dir.path().join("latest.log"))
            .unwrap()
            .contains("persist me")
    );
    logger.shutdown().await.unwrap();
    logger.shutdown().await.unwrap();
}
