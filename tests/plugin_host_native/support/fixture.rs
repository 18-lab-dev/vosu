//! One testkit run: its control file, nonce, receipts and barriers
//! (`crates/vosu-plugin-testkit/examples/testkit_process_plugin/control.rs`
//! is the fixture's side of this format).
//!
//! The control file lives next to the staged library, so it belongs to one
//! test's own package; nothing is shared between tests or keyed by PID
//! across runs. Receipt lines carry the run's nonce, and a line naming any
//! other nonce fails the test.

use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::artifacts::Staged;

/// The fixture's control file name, next to the library.
const CONTROL_FILE: &str = "testkit-fixture.control";

/// One receipt line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub pid: u32,
    pub probe: String,
    pub fields: Vec<(String, String)>,
}

impl Receipt {
    /// The value of `key`. Panics when absent.
    pub fn get(&self, key: &str) -> &str {
        self.fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
            .unwrap_or_else(|| panic!("{key} missing from {self:?}"))
    }
}

/// One run's nonce and receipt directory.
#[derive(Debug)]
pub struct Fixture {
    pub nonce: String,
    pub dir: PathBuf,
}

impl Fixture {
    /// A run with a fresh nonce whose receipts go to `dir` (created).
    pub fn new(dir: &Path) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let nonce = std::collections::hash_map::RandomState::new().hash_one((
            std::process::id(),
            SystemTime::now(),
            dir,
        ));
        Self {
            nonce: format!("{nonce:016x}"),
            dir: dir.to_owned(),
        }
    }

    /// Writes the control file next to `staged`'s library: `fault` (a
    /// `Fault` name), the opcode whose first dispatch fires it, and the
    /// range each attached session reads.
    pub fn control(
        &self,
        staged: &Staged,
        fault: &str,
        trigger: Option<u32>,
        read: Option<(u64, u32)>,
    ) {
        let dir = self.dir.to_str().expect("UTF-8 receipt directory");
        let mut text = format!("nonce={}\ndir={dir}\nfault={fault}\n", self.nonce);
        if let Some(trigger) = trigger {
            text.push_str(&format!("trigger={trigger}\n"));
        }
        if let Some((address, length)) = read {
            text.push_str(&format!("read={address}:{length}\n"));
        }
        std::fs::write(staged.library.parent().unwrap().join(CONTROL_FILE), text).unwrap();
    }

    /// Adds one `key=value` line to the control file [`Self::control`]
    /// wrote next to `staged`'s library (for example `spawn`, M5 N0).
    pub fn append_control(&self, staged: &Staged, key: &str, value: &str) {
        use std::io::Write as _;
        assert!(!value.contains('\n'), "{key} spans lines");
        std::fs::OpenOptions::new()
            .append(true)
            .open(staged.library.parent().unwrap().join(CONTROL_FILE))
            .unwrap()
            .write_all(format!("{key}={value}\n").as_bytes())
            .unwrap();
    }

    /// Every complete receipt line in `<probe>-<pid>` files (all PIDs), in
    /// file order. A line naming another run's nonce fails the test.
    pub fn receipts(&self, probe: &str) -> Vec<Receipt> {
        let prefix = format!("{probe}-");
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&self.dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if let Some(pid) = name.strip_prefix(&prefix).and_then(|pid| pid.parse().ok()) {
                found.extend(self.read(&self.dir.join(&name), pid));
            }
        }
        found
    }

    /// The journal of process `pid`: every receipt it wrote, in order.
    pub fn journal(&self, pid: u32) -> Vec<Receipt> {
        self.read(&self.dir.join(format!("journal-{pid}")), pid)
    }

    fn read(&self, path: &Path, pid: u32) -> Vec<Receipt> {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        // Only `\n`-terminated lines are complete.
        let complete = text.rsplit_once('\n').map_or("", |(lines, _)| lines);
        complete
            .lines()
            .map(|line| {
                let mut fields: Vec<(String, String)> = line
                    .split_whitespace()
                    .filter_map(|pair| pair.split_once('='))
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .collect();
                let mut take = |key: &str| {
                    let at = fields.iter().position(|(name, _)| name == key);
                    at.map(|at| fields.remove(at).1)
                        .unwrap_or_else(|| panic!("{key} missing from receipt {line:?}"))
                };
                assert_eq!(
                    take("nonce"),
                    self.nonce,
                    "a receipt from another run: {line:?}"
                );
                let written_by: u32 = take("pid").parse().unwrap();
                assert_eq!(written_by, pid, "{line:?} is in another process's file");
                Receipt {
                    pid,
                    probe: take("probe"),
                    fields,
                }
            })
            .collect()
    }

    /// Waits up to `bound` for the first `probe` receipt matching `accept`.
    #[expect(
        clippy::disallowed_methods,
        reason = "receipts are files another process writes; the wait polls them until a bounded deadline"
    )]
    pub fn wait_for(
        &self,
        probe: &str,
        bound: Duration,
        accept: impl Fn(&Receipt) -> bool,
    ) -> Option<Receipt> {
        let deadline = Instant::now() + bound;
        loop {
            if let Some(found) = self.receipts(probe).into_iter().find(&accept) {
                return Some(found);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// [`Self::wait_for`] any `probe` receipt.
    pub fn wait(&self, probe: &str, bound: Duration) -> Option<Receipt> {
        self.wait_for(probe, bound, |_| true)
    }

    /// Releases process `pid`'s blocked dispatch.
    pub fn release(&self, pid: u32) {
        std::fs::write(self.dir.join(format!("release-{pid}")), &self.nonce).unwrap();
    }
}
