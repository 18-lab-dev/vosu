//! A real `PluginHost` whose process launcher starts the real `vosu`
//! executable, plus process identities that survive PID reuse.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vosu_core::plugin::process::launcher::ProcessHostLauncher;
use vosu_core::plugin::{self, HostError, LoadedPlugin, PluginHost, PluginInstanceId};
use vosu_plugin_api::Message;
use vosu_plugin_api::memory::{
    MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest,
};

/// The controlled-destroy grace (plugin-abi.md §7.6).
pub const DESTROY_GRACE: Duration = Duration::from_secs(5);

struct Console;

impl vosu_core::logging::ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _text: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _lines: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _text: &str) -> std::io::Result<()> {
        Ok(())
    }
}

struct EmptySettings;

impl vosu_core::plugin::settings::SettingsStorage for EmptySettings {
    fn load(
        &self,
        _plugin_id: &str,
    ) -> Result<
        serde_json::Map<String, serde_json::Value>,
        vosu_core::plugin::settings::SettingsError,
    > {
        Ok(serde_json::Map::new())
    }
    fn store(
        &self,
        _plugin_id: &str,
        _values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), vosu_core::plugin::settings::SettingsError> {
        Ok(())
    }
}

/// Synthetic target memory: `bytes` at address 0.
struct TargetMemory {
    bytes: Vec<u8>,
}

impl ProcessMemory for TargetMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let slice = address
            .checked_add(dst.len())
            .and_then(|end| self.bytes.get(address..end))
            .ok_or_else(|| MemoryError::Read {
                address,
                len: dst.len(),
                message: "outside the synthetic target".to_owned(),
            })?;
        dst.copy_from_slice(slice);
        Ok(())
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0,
            size: self.bytes.len(),
        }])
    }
}

/// A `PluginHost` with the production process launcher installed.
pub struct Host {
    pub host: Arc<PluginHost>,
    runtime: tokio::runtime::Runtime,
    data: PathBuf,
    targets: std::sync::Mutex<Vec<u32>>,
}

/// Runs `vosu plugin-host` once per test process, with no descriptors, which
/// refuses at once (exit 70) without starting a daemon. On Windows the first
/// execution of a never-seen image hash is evaluated before it runs (the
/// VM's Smart App Control evaluation mode: about 3 s for a fresh 31 MB
/// executable). Without this, every test in a run's first parallel wave paid
/// that one-time cost for the freshly built `vosu.exe` inside the plugin
/// host's 5-second startup deadline: they all took 3.3–4.3 s to start, against
/// about 60 ms afterwards.
fn prime_executable() {
    static PRIMED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    PRIMED.get_or_init(|| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_vosu"))
            .arg("plugin-host")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(70), "{output:?}");
    });
}

impl Host {
    /// A host whose logs and instance data live under `root`.
    pub fn new(root: &Path) -> Self {
        Self::with_launcher(root, |launcher| launcher)
    }

    /// [`Self::new`] with the production launcher adjusted by `configure`,
    /// for example to shorten its startup deadlines.
    pub fn with_launcher(
        root: &Path,
        configure: impl FnOnce(ProcessHostLauncher) -> ProcessHostLauncher,
    ) -> Self {
        prime_executable();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = {
            let _enter = runtime.enter();
            let logger = vosu_core::logging::Logger::new(
                root.join("logs"),
                vosu_core::logging::LoggerOptions {
                    version: "0.1.0".to_owned(),
                    debug_log: false,
                    queue_capacity: 64,
                },
                Arc::new(Console),
            )
            .unwrap();
            let settings = Arc::new(vosu_core::plugin::settings::PluginSettingsService::new(
                Arc::new(EmptySettings),
            ));
            PluginHost::new(logger, settings, runtime.handle().clone())
        };
        let launcher = ProcessHostLauncher::with_executable(
            Path::new(env!("CARGO_BIN_EXE_vosu")),
            vosu::plugin_host::BUILD_IDENTITY,
        )
        .unwrap();
        host.install_process_launcher(Arc::new(configure(launcher)))
            .unwrap();
        Self {
            host,
            runtime,
            data: root.join("data"),
            targets: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Validates the staged package in `dir` as a development package.
    pub fn load(&self, dir: &Path) -> LoadedPlugin {
        plugin::load_development(
            vosu::application_version(),
            &vosu_core::plugin::ValidationSnapshot::default(),
            dir,
            true,
        )
        .unwrap_or_else(|error| panic!("{} does not validate: {error}", dir.display()))
    }

    /// Creates instance `id` of `loaded`, returning once the child reached
    /// `READY` or failed.
    pub fn create(&self, id: u64, loaded: &LoadedPlugin) -> Result<(), HostError> {
        let _enter = self.runtime.enter();
        self.host.create_instance(
            PluginInstanceId::new(id),
            loaded,
            self.data.join(id.to_string()),
            None,
        )
    }

    pub fn dispatch(&self, id: u64, message: &Message) -> Result<(), HostError> {
        let _enter = self.runtime.enter();
        self.host
            .dispatch_to_instance(PluginInstanceId::new(id), message, 0)
            .map(drop)
    }

    /// Waits up to `bound` until instance `id` refuses deliveries.
    #[expect(
        clippy::disallowed_methods,
        reason = "instance retirement happens on core's supervision threads; the wait polls it until a bounded deadline"
    )]
    pub fn wait_retired(&self, id: u64, message: &Message, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        loop {
            if matches!(self.dispatch(id, message), Err(HostError::UnknownInstance)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn destroy_all(&self) {
        let _enter = self.runtime.enter();
        self.host.destroy_all(DESTROY_GRACE);
    }

    /// Offers a synthetic target process `pid` named `name` whose memory is
    /// `bytes` at address 0. The PID must be unique within this test binary.
    pub fn offer_target(&self, pid: u32, name: &str, bytes: Vec<u8>) {
        self.hook_memory(pid, Arc::new(TargetMemory { bytes }), ProcessBitness::X64);
        self.offer_process(pid, name, vosu_plugin_api::Bitness::X64);
    }

    /// Makes core read `pid` through `memory` instead of opening it itself,
    /// until this host is dropped. The PID must be unique within this test
    /// binary: a live target process's own PID is.
    pub fn hook_memory(&self, pid: u32, memory: Arc<dyn ProcessMemory>, bitness: ProcessBitness) {
        vosu_core::plugin::test_support::set_session_memory(pid, memory, bitness);
        self.targets.lock().unwrap().push(pid);
    }

    /// Reports process `pid` named `name` to the matcher. Unless
    /// [`Self::hook_memory`] hooked it, core opens and reads the real
    /// process `pid` itself.
    pub fn offer_process(&self, pid: u32, name: &str, bitness: vosu_plugin_api::Bitness) {
        let _enter = self.runtime.enter();
        self.host
            .process_appeared(vosu_core::plugin::ProcessRecord {
                pid,
                name: name.to_owned(),
                path: format!("/fixture/{name}"),
                command_line: name.to_owned(),
                bitness,
                start_time: 1,
                argv: None,
            });
    }

    /// Reports that process `pid` exited.
    pub fn process_exited(&self, pid: u32) {
        let _enter = self.runtime.enter();
        self.host.process_exited(pid);
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.destroy_all();
        for pid in self.targets.lock().unwrap().drain(..) {
            vosu_core::plugin::test_support::clear_session_memory(pid);
        }
    }
}

/// A process identity that survives PID reuse: the PID plus its start time
/// (Linux `/proc/<pid>/stat` field 22), or a live process handle (Windows).
#[cfg(target_os = "linux")]
pub struct Identity {
    pid: u32,
    start: String,
}

#[cfg(target_os = "linux")]
impl Identity {
    pub fn of(pid: u32) -> Self {
        let start = Self::stat_field(pid, 19).expect("the plugin host is alive when observed");
        Self { pid, start }
    }

    /// Field `index` after the command name of `/proc/<pid>/stat` (0 is the
    /// state, 19 the start time).
    fn stat_field(pid: u32, index: usize) -> Option<String> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_name = &stat[stat.rfind(')')? + 2..];
        after_name.split_whitespace().nth(index).map(str::to_owned)
    }

    /// Whether this exact process still exists, unreaped zombies included.
    pub fn alive(&self) -> bool {
        Self::stat_field(self.pid, 19).as_deref() == Some(self.start.as_str())
    }

    /// Whether this exact process is stopped (state `T`).
    pub fn stopped(&self) -> bool {
        self.alive() && Self::stat_field(self.pid, 0).as_deref() == Some("T")
    }
}

#[cfg(windows)]
pub struct Identity {
    pid: u32,
    handle: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl Identity {
    pub fn of(pid: u32) -> Self {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };
        // SAFETY: plain call; a null result is checked before ownership.
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        assert!(!raw.is_null(), "the plugin host is alive when observed");
        // SAFETY: `OpenProcess` returned a new handle this value owns.
        let handle = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) };
        Self { pid, handle }
    }

    pub fn alive(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        // SAFETY: waits zero milliseconds on a live owned handle.
        unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) == WAIT_TIMEOUT }
    }

    /// Whether this exact process is alive and every one of its threads is
    /// suspended, read through each thread's own suspend count.
    pub fn stopped(&self) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        };
        use windows_sys::Win32::System::Threading::{
            OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME,
        };
        if !self.alive() {
            return false;
        }
        // SAFETY: a thread snapshot; the handle is closed below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        assert_ne!(snapshot, INVALID_HANDLE_VALUE);
        // SAFETY: THREADENTRY32 is plain integers, for which all-zero is valid.
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let (mut threads, mut suspended) = (0, 0);
        // SAFETY: `entry` is a writable THREADENTRY32 with `dwSize` set.
        let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == self.pid {
                threads += 1;
                // SAFETY: plain call; a null result is skipped.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !thread.is_null() {
                    // SAFETY: `thread` is a live handle with suspend/resume
                    // access; the extra suspension is undone at once, so
                    // only the previous count is observed.
                    unsafe {
                        if SuspendThread(thread) != u32::MAX {
                            let previous = ResumeThread(thread);
                            if previous != u32::MAX && previous > 1 {
                                suspended += 1;
                            }
                        }
                        CloseHandle(thread);
                    }
                }
            }
            // SAFETY: as for `Thread32First`.
            more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        // SAFETY: closes the snapshot handle opened above.
        unsafe { CloseHandle(snapshot) };
        threads > 0 && suspended == threads
    }
}

/// Waits up to `bound` for `condition`.
#[expect(
    clippy::disallowed_methods,
    reason = "process state belongs to another process; the wait polls it until a bounded deadline"
)]
pub fn wait_until(bound: Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + bound;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
