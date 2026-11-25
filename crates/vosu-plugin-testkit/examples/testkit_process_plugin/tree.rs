//! M5 N0 additions: a descendant process for the core-death trees, the
//! security readback after this plugin's own thread exists, and the
//! [`crate::control::Fault::WriteBlock`] flood (`docs/testing/verification.md`, "M5 test
//! and fixture conventions" and "N0 packaged Linux drop procedure").

use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Instant;

use vosu_plugin_api::{
    CborValue, HostServices, LogLevel, LogRecord, Message, VtableHostServices, vosu_status,
};

use crate::control::{Control, WRITE_BLOCKED_AFTER};
use crate::native;

/// One flood record's message: 32 KiB, so about 64 records fill the 2 MiB
/// plugin → host ring.
const FLOOD_MESSAGE_BYTES: usize = 32 * 1024;

/// Starts the control's descendant with null standard handles and writes
/// its `descendant` receipt (`child=<pid>`), or `descendant_failed`.
#[expect(
    clippy::disallowed_methods,
    reason = "the fixture is plugin code: a plugin may start its own helper processes, and the core-death trees need one"
)]
pub fn spawn_descendant(control: &Control, argv: &[String]) -> Option<Child> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    native::without_console(&mut command);
    match command.spawn() {
        Ok(child) => {
            control.receipt("descendant", &format!("child={}", child.id()));
            Some(child)
        }
        Err(error) => {
            control.receipt(
                "descendant_failed",
                &format!("error={}", error.to_string().replace(' ', "_")),
            );
            None
        }
    }
}

/// Starts a thread of this plugin's own (it parks for the life of the
/// process), then writes the `security` receipt with the readback taken
/// while that thread exists: the "after the plugin has created its
/// threads" observation.
pub fn security_probe(control: &Control) {
    let (started, running) = std::sync::mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("testkit-probe".to_owned())
        .spawn(move || {
            let _ = started.send(());
            loop {
                std::thread::park();
            }
        });
    let thread = match spawned {
        Ok(_) if running.recv().is_ok() => "started",
        _ => "failed",
    };
    control.receipt(
        "security",
        &format!("plugin_thread={thread} {}", native::readback()),
    );
}

/// What the flood tells its watchdog about one send.
enum Send {
    Started { sequence: u64, at: Instant },
    Finished,
}

/// [`crate::control::Fault::WriteBlock`]: a flood thread that waits for
/// the test's release, and a watchdog that reports one send blocked.
pub fn start_write_block(host: VtableHostServices, control: &'static Control) {
    let (sends, watched) = mpsc::sync_channel(2);
    let flood = std::thread::Builder::new()
        .name("testkit-flood".to_owned())
        .spawn(move || flood(host, control, &sends));
    let watchdog = std::thread::Builder::new()
        .name("testkit-watchdog".to_owned())
        .spawn(move || watchdog(control, &watched));
    if flood.is_err() || watchdog.is_err() {
        control.receipt("write_flood_failed", "");
    }
}

fn flood(host: VtableHostServices, control: &Control, sends: &mpsc::SyncSender<Send>) {
    if !control.wait_release() {
        return;
    }
    control.receipt("write_flood", "");
    let record = Message::LogRecord(LogRecord {
        level: LogLevel::Info,
        message: "n".repeat(FLOOD_MESSAGE_BYTES),
        fields: CborValue::Map(Vec::new()),
    });
    for sequence in 1_u64.. {
        let at = Instant::now();
        if sends.send(Send::Started { sequence, at }).is_err() {
            return;
        }
        let status = host.send(&record, 0);
        let waited = at.elapsed();
        if status != vosu_status::VOSU_OK {
            control.receipt(
                "write_refused",
                &format!("seq={sequence} status={}", status as i32),
            );
            return;
        }
        if waited >= WRITE_BLOCKED_AFTER {
            control.receipt(
                "write_returned",
                &format!("seq={sequence} waited_ms={}", waited.as_millis()),
            );
            return;
        }
        if sends.send(Send::Finished).is_err() {
            return;
        }
    }
}

/// Waits for each send to finish within [`WRITE_BLOCKED_AFTER`], and
/// writes `write_blocked` for the first one that does not.
fn watchdog(control: &Control, watched: &mpsc::Receiver<Send>) {
    while let Ok(Send::Started { sequence, at }) = watched.recv() {
        match watched.recv_timeout(WRITE_BLOCKED_AFTER) {
            Ok(Send::Finished) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                control.receipt(
                    "write_blocked",
                    &format!("seq={sequence} waited_ms={}", at.elapsed().as_millis()),
                );
                return;
            }
            Ok(Send::Started { .. }) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}
