//! M5 N0 additions: a descendant process for the core-death trees, the
//! security readback after this plugin's own thread exists, and the
//! [`Fault::WriteBlock`] flood (`docs/testing/verification.md`, "M5 test
//! and fixture conventions" and "N0 packaged Linux drop procedure").

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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

/// Milliseconds since `base` plus one while a flood send is in progress;
/// 0 while none is.
static SENDING: AtomicU64 = AtomicU64::new(0);
/// The sequence number of the send in progress.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// [`crate::control::Fault::WriteBlock`]: a flood thread that waits for
/// the test's release, and a watchdog that reports one send blocked.
pub fn start_write_block(host: VtableHostServices, control: &'static Control) {
    let base = Instant::now();
    let flood = std::thread::Builder::new()
        .name("testkit-flood".to_owned())
        .spawn(move || flood(host, control, base));
    let watchdog = std::thread::Builder::new()
        .name("testkit-watchdog".to_owned())
        .spawn(move || watchdog(control, base));
    if flood.is_err() || watchdog.is_err() {
        control.receipt("write_flood_failed", "");
    }
}

fn flood(host: VtableHostServices, control: &Control, base: Instant) {
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
        let started = Instant::now();
        SEQUENCE.store(sequence, Ordering::Release);
        SENDING.store(millis(base) + 1, Ordering::Release);
        let status = host.send(&record, 0);
        SENDING.store(0, Ordering::Release);
        let waited = started.elapsed();
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
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the watchdog samples another thread's send start until one send has been blocked long enough; no wake source exists for a send that never returns"
)]
fn watchdog(control: &Control, base: Instant) {
    loop {
        std::thread::sleep(Duration::from_millis(5));
        let sending = SENDING.load(Ordering::Acquire);
        if sending == 0 {
            continue;
        }
        let waited = millis(base).saturating_sub(sending - 1);
        if waited >= WRITE_BLOCKED_AFTER.as_millis() as u64 {
            control.receipt(
                "write_blocked",
                &format!(
                    "seq={} waited_ms={waited}",
                    SEQUENCE.load(Ordering::Acquire)
                ),
            );
            return;
        }
    }
}

fn millis(base: Instant) -> u64 {
    base.elapsed().as_millis() as u64
}
