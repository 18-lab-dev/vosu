//! Per-run fixture control and receipts.
//!
//! The control file [`CONTROL_FILE`] sits next to the staged library, so it
//! belongs to one test's package and run. The load-time constructor finds
//! it through its own module path, before `create` and without any
//! environment variable or PID-keyed registry. Its lines are `key=value`:
//!
//! - `nonce`: required; copied into every receipt, so a receipt from
//!   another run can never satisfy a wait.
//! - `dir`: required; the absolute directory receipts and barriers live in.
//! - `fault`: one of [`Fault::NAMES`] (default `none`).
//! - `trigger`: the opcode whose first dispatch fires a dispatch-time
//!   fault; required for those faults.
//! - `read`: `<address>:<length>` read from each attached session
//!   (default `0:8`).
//!
//! Receipts are appended as one line each to `<probe>-<pid>` and, in order,
//! to `journal-<pid>` in `dir`. A line is
//! `nonce=<nonce> pid=<pid> probe=<probe> <key=value>...` and ends in
//! `\n`, so a reader ignores a partially written line.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The control file's name, next to the library.
pub const CONTROL_FILE: &str = "testkit-fixture.control";

/// How long a blocked dispatch waits for its release before it gives up.
const RELEASE_BOUND: Duration = Duration::from_secs(120);

/// The fault this run injects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    None,
    /// The load-time constructor aborts the process.
    ConstructorCrash,
    /// `create` aborts the process.
    CreateCrash,
    /// The triggering dispatch aborts the process.
    DispatchCrash,
    /// The triggering dispatch waits for `release-<pid>`.
    DispatchBlock,
    /// `ACTION_INVOKE` never completes and `ACTION_CANCEL` is ignored.
    CancelIgnored,
    /// `destroy` never returns.
    DestroyIgnored,
    /// The triggering dispatch stops the whole process, so its ring reader
    /// stops draining.
    RingStall,
    /// The triggering dispatch publishes a malformed frame into the plugin →
    /// host ring and wakes core.
    CorruptFrame,
}

impl Fault {
    pub const NAMES: [(&'static str, Self); 9] = [
        ("none", Self::None),
        ("constructor_crash", Self::ConstructorCrash),
        ("create_crash", Self::CreateCrash),
        ("dispatch_crash", Self::DispatchCrash),
        ("dispatch_block", Self::DispatchBlock),
        ("cancel_ignored", Self::CancelIgnored),
        ("destroy_ignored", Self::DestroyIgnored),
        ("ring_stall", Self::RingStall),
        ("corrupt_frame", Self::CorruptFrame),
    ];

    fn parse(name: &str) -> Option<Self> {
        Self::NAMES
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, fault)| *fault)
    }

    /// Whether the fault fires on its trigger's first dispatch.
    pub fn fires_in_dispatch(self) -> bool {
        matches!(
            self,
            Self::DispatchCrash | Self::DispatchBlock | Self::RingStall | Self::CorruptFrame
        )
    }
}

/// One run's control, read once at load.
#[derive(Debug)]
pub struct Control {
    pub nonce: String,
    pub dir: PathBuf,
    pub fault: Fault,
    pub trigger: Option<u32>,
    pub read: (u64, u32),
}

static CONTROL: OnceLock<Option<Control>> = OnceLock::new();

/// This load's control, if the constructor found a valid one.
pub fn get() -> Option<&'static Control> {
    CONTROL.get().and_then(Option::as_ref)
}

/// Runs once, from the load-time constructor: reads the control file next
/// to `library`, writes the `constructor` receipt with `readback`, and
/// aborts for [`Fault::ConstructorCrash`]. Without a valid control file it
/// does nothing, and `create` refuses.
pub fn on_load(library: Option<PathBuf>, readback: &str) {
    let control = library.as_deref().and_then(read_control);
    let control = CONTROL.get_or_init(|| control).as_ref();
    let Some(control) = control else {
        return;
    };
    let library = library
        .as_deref()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    control.receipt(
        "constructor",
        &format!("library={} {readback}", library.replace(' ', "_")),
    );
    if control.fault == Fault::ConstructorCrash {
        std::process::abort();
    }
}

fn read_control(library: &Path) -> Option<Control> {
    let text = std::fs::read_to_string(library.parent()?.join(CONTROL_FILE)).ok()?;
    let mut nonce = None;
    let mut dir = None;
    let mut fault = Fault::None;
    let mut trigger = None;
    let mut read = (0, 8);
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let (key, value) = line.split_once('=')?;
        match key {
            "nonce" if !value.is_empty() && !value.contains(char::is_whitespace) => {
                nonce = Some(value.to_owned());
            }
            "dir" if Path::new(value).is_absolute() => dir = Some(PathBuf::from(value)),
            "fault" => fault = Fault::parse(value)?,
            "trigger" => trigger = Some(value.parse().ok()?),
            "read" => {
                let (address, length) = value.split_once(':')?;
                read = (address.parse().ok()?, length.parse().ok()?);
            }
            // An unknown or malformed key is a test setup error: refuse
            // the whole control rather than run a different scenario.
            _ => return None,
        }
    }
    if fault.fires_in_dispatch() && trigger.is_none() {
        return None;
    }
    Some(Control {
        nonce: nonce?,
        dir: dir?,
        fault,
        trigger,
        read,
    })
}

impl Control {
    /// Appends one receipt line for `probe` to `<probe>-<pid>` and to the
    /// journal. A failed write leaves the receipt absent, which the test
    /// reports.
    pub fn receipt(&self, probe: &str, fields: &str) {
        let pid = std::process::id();
        let line = format!("nonce={} pid={pid} probe={probe} {fields}\n", self.nonce);
        for name in [format!("{probe}-{pid}"), format!("journal-{pid}")] {
            let _ = append(&self.dir.join(name), line.as_bytes());
        }
    }

    /// Waits for the test's `release-<pid>` barrier file. Returns whether it
    /// appeared within [`RELEASE_BOUND`].
    #[expect(
        clippy::disallowed_methods,
        reason = "a blocked fixture dispatch polls the test's barrier file until a bounded deadline; no other wake source crosses the process boundary"
    )]
    pub fn wait_release(&self) -> bool {
        let release = self.dir.join(format!("release-{}", std::process::id()));
        let deadline = Instant::now() + RELEASE_BOUND;
        loop {
            if std::fs::read_to_string(&release).is_ok_and(|text| text.trim() == self.nonce) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn append(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(bytes)
}
